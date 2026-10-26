//! Admission, visibility and structural-skip behavior.

use super::super::redraw_tests::*;
use super::*;

/// A due child cannot wake main or a later child; maintenance ties never suppress a coincident repaint.
#[test]
fn typed_due_service_wakes_only_due_owners_and_keeps_maintenance_ties() {
    let (mut app, main, child) = owners();
    let other = app.__test_seed_child_window(&["later"]);
    let now = Instant::now();
    app.redraw_due = vec![
        DueWork { owner: Some(child), cause: DueCause::Frame, deadline: now },
        DueWork { owner: Some(child), cause: DueCause::Cursor, deadline: now },
        DueWork { owner: None, cause: DueCause::Memory, deadline: now },
        DueWork { owner: None, cause: DueCause::PointerMotion, deadline: now },
        DueWork {
            owner: Some(other),
            cause: DueCause::Frame,
            deadline: now + Duration::from_secs(1),
        },
    ];
    app.service_redraw_due(now);
    assert!(!app.windows[&main].redraw.request_in_flight);
    assert!(app.windows[&child].redraw.request_in_flight);
    assert!(!app.windows[&other].redraw.request_in_flight);
    assert_eq!(app.redraw_due.len(), 1);
    assert_eq!(app.redraw_due[0].owner, Some(other));
    let snapshot = app.windows[&child].redraw.snapshot();
    app.service_redraw_due(now);
    assert_eq!(
        app.windows[&child].redraw.snapshot().0,
        snapshot.0,
        "serviced entries do not fire twice"
    );
}

/// Drawn, retried and failed input attempts spend input once, keep dirt off the timer, and pace from the attempt.
#[test]
fn repeated_outcomes_spend_input_immediacy_without_dirty_row_heartbeats() {
    for outcome in [
        FrameSettlement::Cached,
        FrameSettlement::Presented,
        FrameSettlement::Retry(RedrawCause::SurfaceRetry),
        FrameSettlement::Failed,
    ] {
        assert_paced_repeat(RedrawCause::Input, outcome);
    }
}

/// A settled attempt without a new input generation keeps every pacing guarantee of a drawn one.
#[test]
fn settled_attempt_without_new_input_stays_paced_without_heartbeats() {
    assert_paced_repeat(RedrawCause::Expose, FrameSettlement::Settled);
}

/// A settled hardware keypress spends its input without a heartbeat, keeps the streaming clock, and
/// the next non-input completion moves that clock again and restores streaming deferral.
#[test]
fn settled_keypress_keeps_the_streaming_clock_until_the_next_non_input_completion() {
    use super::super::super::frame_counters::DeferRule;
    let period = Duration::from_micros(16_667);
    for following in [FrameSettlement::Presented, FrameSettlement::Settled] {
        let (mut app, main, _) = counting_owners();
        let pane = app.windows[&main].tab_states[0].active_pane;
        app.windows[&main].panes[&pane].parser.lock().grid_mut().mark_all_dirty();
        let now = Instant::now() + Duration::from_secs(1);
        let backdated = now - period * 2;
        backdate_clocks(&mut app, main, backdated);
        app.mark_window_redraw(main, RedrawCause::Input);
        let snapshot = app.snapshot_window_redraw(main).unwrap();
        app.finish_window_redraw(main, &snapshot, FrameSettlement::Settled, now);
        assert_eq!(app.windows[&main].last_render, now);
        assert!(!app.windows[&main].redraw.input_pending());
        assert!(app.windows[&main].panes[&pane].parser.lock().grid().dirty_count() > 0);
        assert!(app.frame_due_work().is_empty(), "dirt or retained causes alone never arm a timer");
        assert_eq!(app.windows[&main].stream_clock, backdated, "the keypress drew nothing");
        let exempt = |app: &App| {
            app.windows[&main].redraw.frame_counters.as_deref().unwrap().stream_clock_exempt
        };
        assert_eq!(exempt(&app), 1);
        // The echo lands one microsecond later and is admitted at once.
        let echo_at = now + Duration::from_micros(1);
        publish_visible_output(&app, main);
        app.mark_window_redraw(main, RedrawCause::Output);
        assert!(app.begin_window_redraw(main, echo_at), "the echo is not paced from the keypress");
        let snapshot = app.snapshot_window_redraw_at(main, echo_at).unwrap();
        app.finish_window_redraw(main, &snapshot, following, echo_at);
        assert_eq!(app.windows[&main].stream_clock, echo_at, "{following:?} moves the clock");
        assert_eq!(exempt(&app), 1, "only input attempts are exempt");
        // Further output is streaming work, paced one period from the echo frame.
        publish_visible_output(&app, main);
        app.mark_window_redraw(main, RedrawCause::Output);
        let probe = echo_at + Duration::from_micros(1);
        assert!(!app.begin_window_redraw(main, probe));
        assert_eq!(defer_count(&app, main, DeferRule::Streaming), 1);
        let frames: Vec<_> = app
            .frame_due_work_at(probe)
            .into_iter()
            .filter(|work| work.owner == Some(main) && work.cause == DueCause::Frame)
            .collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].deadline, echo_at + period);
    }
}

/// Structural parking suppresses all owner frame deadlines; output maintenance still runs and only topology-capable causes unpark.
#[test]
fn structural_park_keeps_output_maintenance_without_repainting_and_unparks_once() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    let pane = app.windows[&child].tab_states[1].active_pane;
    app.windows.get_mut(&child).unwrap().panes.get_mut(&pane).unwrap().command_events.lock().push(
        crate::app::PaneCommandEvent {
            event: sonicterm_vt::vt::CommandEvent::CmdStart,
            at: now,
            duration: None,
        },
    );
    let state = app.windows.get_mut(&child).unwrap();
    state.redraw.mark(RedrawCause::Input);
    let snapshot = state.redraw.snapshot();
    state.redraw.park(snapshot);
    state.redraw.deferred = true;
    state.retry_not_before = Some(now + Duration::from_millis(1));
    app.output_redraw_notification(child, now);
    assert!(app.windows[&child].redraw.parked);
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert!(matches!(app.windows[&child].tabs.tabs()[1].command, CommandStatus::Running(_)));
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(child)));
    assert!(!app.windows[&main].redraw.has_pending());
    app.request_owner_redraw(child, RedrawCause::Chrome);
    assert!(app.windows[&child].redraw.parked);
    app.request_owner_redraw(child, RedrawCause::Topology);
    assert!(!app.windows[&child].redraw.parked);
    assert!(app.windows[&child].redraw.request_in_flight);
    let captured = app.windows[&child].redraw.snapshot();
    app.request_owner_redraw(child, RedrawCause::Output);
    assert!(app.windows[&child].redraw.request_in_flight);
    assert_ne!(
        app.windows[&child].redraw.snapshot().0,
        captured.0,
        "later output remains a new cause"
    );
}

/// A stopped owner is reported before topology suppression and neither frame dirt nor worker output can schedule it.
#[test]
fn stopped_and_parked_owners_are_excluded_from_the_deadline_fold() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    for id in [main, child] {
        let window = app.windows.get_mut(&id).unwrap();
        window.redraw.deferred = true;
        window.last_render = now - Duration::from_secs(1);
        window.retry_not_before = Some(now);
    }
    app.windows.get_mut(&main).unwrap().redraw.stopped_generation = Some(17);
    app.windows.get_mut(&child).unwrap().redraw.parked = true;
    assert!(app.frame_due_work().is_empty());
    let source = concat!(
        include_str!("redraw.rs"),
        "\n",
        include_str!("redraw_admission.rs"),
        "\n",
        include_str!("redraw_state.rs"),
        "\n",
        include_str!("redraw_window.rs")
    );
    let entry = source.find("fn begin_window_redraw(").unwrap();
    let report = source[entry..].find("take_stopped_render_outcome()").unwrap();
    let parked = source[entry..].find("!window.frame_deadlines_allowed()").unwrap();
    assert!(report < parked);
}

/// Real role adapters consume the callback's typed snapshot and inspect outcomes before result conversion.
#[test]
fn production_roles_preserve_prelock_snapshot_and_exact_attempt_accounting() {
    for source in [
        concat!(
            concat!(
                include_str!("window_event.rs"),
                "\n",
                include_str!("window_event_pointer.rs"),
                "\n",
                include_str!("window_event_redraw.rs")
            ),
            concat!(
                include_str!("window_keyboard.rs"),
                "\n",
                include_str!("window_keyboard_ime.rs")
            ),
            include_str!("splitter_input.rs"),
            include_str!("window_pointer.rs")
        ),
        concat!(
            include_str!("child_window.rs"),
            include_str!("child_tabs.rs"),
            include_str!("splitter_input.rs"),
            include_str!("child_window_pointer.rs"),
            include_str!("child_window_redraw.rs")
        ),
    ] {
        assert!(source.contains("sources.try_collect(|| self.snapshot_window_redraw(win_id))"));
        // The call binds its outcome and the presented frame's receipts by destructuring.
        let render = source.find("r.render_releasing(").unwrap();
        let binding =
            source[..render].rfind("let sonicterm_gpu::core::FrameOutcome { outcome, receipts } =");
        assert!(binding.is_some_and(|binding| render - binding < 80));
        let classify = source[render..].find("FrameSettlement::of(&outcome)").unwrap();
        let consume = source[render..].find("outcome.into_render_result()").unwrap();
        assert!(classify < consume);
        assert_eq!(source.matches("smoke.note_render_attempt();").count(), 1);
        let recovery = source[render..].find("recovery.observe_frame(").unwrap();
        let smoke = source[render..].find("smoke.observe_recovery_frame(").unwrap();
        assert!(recovery < consume && smoke < consume);
        assert!(!source.contains("self.input_dirty") && !source.contains("self.pty_burst_gen"));
    }
}

/// Hidden and stopped owners still drain command events, while no Output notification can wake their frames.
#[test]
fn command_maintenance_runs_without_hidden_or_stopped_frame_requests() {
    for stopped in [false, true] {
        let (mut app, _, child) = owners();
        let now = Instant::now();
        let state = app.windows.get_mut(&child).unwrap();
        state.hidden = !stopped;
        if stopped {
            state.redraw.stopped_generation = Some(23);
        }
        let pane = state.tab_states[1].active_pane;
        state.panes[&pane].command_events.lock().push(crate::app::PaneCommandEvent {
            event: sonicterm_vt::vt::CommandEvent::CmdEnd(Some(0)),
            at: now,
            duration: None,
        });
        app.output_redraw_notification(child, now);
        let state = &app.windows[&child];
        assert!(state.panes[&pane].command_events.lock().is_empty());
        assert!(matches!(state.tabs.tabs()[1].command, CommandStatus::Done { exit: Some(0), .. }));
        assert!(!state.redraw.request_in_flight);
        assert!(app.frame_due_work().iter().all(|entry| entry.owner != Some(child)));
    }
}

/// An in-flight native frame suppresses duplicate Frame work, not due UI expiration or a later owner's timers.
#[test]
fn in_flight_notification_and_scrollbar_expire_once_without_rearming_or_touching_later_owner() {
    use crate::app::scrollbar_visibility::{ScrollbarVisState, IDLE_HIDE_MS};
    for child_due in [false, true] {
        for (notification, scrollbar) in [(true, false), (false, true), (true, true)] {
            let (mut app, main, child) = owners();
            let (owner, later) = if child_due { (child, main) } else { (main, child) };
            app.software_render_degrade = true;
            app.config.appearance.scrollbar = sonicterm_cfg::config::ScrollbarMode::Auto;
            let started = Instant::now();
            let now = started + Duration::from_millis(IDLE_HIDE_MS);
            let later_at = now + Duration::from_millis(200);
            for (id, expires) in [(owner, now), (later, later_at)] {
                let window = app.windows.get_mut(&id).unwrap();
                if notification {
                    window.notification = Some(NotificationBubble {
                        level: NotificationLevel::Info,
                        message: format!("owner {id:?}"),
                        expires_at: Some(expires),
                    });
                }
                if scrollbar {
                    let pane = window.tab_states[0].active_pane;
                    let active = expires - Duration::from_millis(IDLE_HIDE_MS);
                    let mut vis = ScrollbarVisState::new(active);
                    crate::app::scrollbar_visibility::note_activity(&mut vis, active);
                    crate::app::scrollbar_visibility::retarget(
                        &mut vis,
                        sonicterm_cfg::config::ScrollbarMode::Auto,
                        false,
                        active,
                    );
                    vis.alpha = 1.0;
                    window.scrollbar_vis.insert(pane, vis);
                }
            }
            let window = app.windows.get_mut(&owner).unwrap();
            window.redraw.deferred = true;
            window.redraw.request_in_flight = true;
            let before = window.redraw.snapshot();
            let later_before = app.windows[&later].redraw.snapshot();
            let later_bubble = app.windows[&later].notification.clone();
            let later_vis = app.windows[&later].scrollbar_vis.clone();
            let due = app.frame_due_work();
            assert!(!due
                .iter()
                .any(|work| work.owner == Some(owner) && work.cause == DueCause::Frame));
            assert_eq!(
                due.iter().filter(|work| work.owner == Some(owner) && work.deadline == now).count(),
                usize::from(notification) + usize::from(scrollbar)
            );
            app.redraw_due = due;
            app.service_redraw_due(now);
            let window = &app.windows[&owner];
            assert!(window.redraw.request_in_flight, "keep the existing native request");
            assert!(window.notification.is_none());
            if scrollbar {
                let pane = window.tab_states[0].active_pane;
                let vis = window.scrollbar_vis[&pane];
                // The deadline is consumed, not erased: activity stays recorded.
                assert_eq!(vis.alpha, 0.0);
                assert!(vis.idle_consumed);
                assert_eq!(vis.last_tick, now);
            }
            let after = window.redraw.snapshot();
            for (cause, present) in
                [(RedrawCause::Chrome, notification), (RedrawCause::Scrollbar, scrollbar)]
            {
                assert_eq!(after.0[cause as usize], before.0[cause as usize] + u64::from(present));
            }
            assert_eq!(app.windows[&later].redraw.snapshot().0, later_before.0);
            assert_eq!(app.windows[&later].notification, later_bubble);
            assert!(!app.windows[&later].redraw.request_in_flight);
            for (pane, before) in &later_vis {
                let after = app.windows[&later].scrollbar_vis[pane];
                assert_eq!(
                    (after.alpha, after.last_active, after.last_tick),
                    (before.alpha, before.last_active, before.last_tick)
                );
            }
            assert!(app
                .redraw_due
                .iter()
                .all(|work| work.owner == Some(later) && work.deadline == later_at));
            app.redraw_due = app.frame_due_work();
            assert!(app
                .redraw_due
                .iter()
                .all(|work| work.owner == Some(later) && work.deadline == later_at));
            app.service_redraw_due(now);
            assert_eq!(
                app.windows[&owner].redraw.snapshot().0,
                after.0,
                "expired entries cannot rearm"
            );
            assert_eq!(app.windows[&later].redraw.snapshot().0, later_before.0);
        }
    }
}

/// Actual main and child close-tab completion must unpark via Topology, not an unrelated input side effect.
#[test]
fn parked_close_tab_adapters_mark_only_the_surviving_owner_topology() {
    for close_child in [false, true] {
        let (mut app, main, child) = topology_owners();
        let (owner, other) = if close_child { (child, main) } else { (main, child) };
        let removed = app.windows[&owner].tab_states[0].active_pane;
        let survivor = app.windows[&owner].tabs.tabs()[1].id;
        let before = park_owner(&mut app, owner);
        park_owner(&mut app, other);
        let other_before = app.windows[&other].redraw.snapshot();
        if close_child {
            assert!(app.close_tab_at_in_child(owner, 0));
        } else {
            app.close_tab_at(0);
        }
        let window = &app.windows[&owner];
        assert!(!window.redraw.parked);
        assert_eq!(window.redraw.snapshot().0[RedrawCause::Topology as usize], before + 1);
        assert_eq!(window.tabs.tabs()[0].id, survivor);
        assert!(!window.panes.contains_key(&removed));
        assert!(app.windows[&other].redraw.parked);
        assert_eq!(app.windows[&other].redraw.snapshot().0, other_before.0);
    }
}

/// Clean sole-pane and split-pane exits use their production dispatch/resize paths to unpark either role.
#[test]
fn parked_pane_exit_adapters_complete_topology_once_for_the_real_owner() {
    for child_exit in [false, true] {
        for split in [false, true] {
            let (mut app, main, child) = topology_owners();
            let (owner, other) = if child_exit { (child, main) } else { (main, child) };
            let original = app.windows[&owner].tab_states[0].active_pane;
            if split {
                if child_exit {
                    assert!(app.__test_invoke_split_active_pane_in_child(
                        owner,
                        sonicterm_cfg::keymap::Direction::Right
                    ));
                } else {
                    app.__test_split_active_right();
                }
            }
            let exiting = app.windows[&owner].tab_states[0].active_pane;
            let before = park_owner(&mut app, owner);
            park_owner(&mut app, other);
            let other_before = app.windows[&other].redraw.snapshot();
            app.handle_pane_process_exited(exiting, sonicterm_types::PaneExitStatus::Code(0));
            let window = &app.windows[&owner];
            assert!(!window.redraw.parked);
            assert_eq!(window.redraw.snapshot().0[RedrawCause::Topology as usize], before + 1);
            assert!(!window.panes.contains_key(&exiting));
            assert_eq!(window.tabs.len(), if split { 2 } else { 1 });
            if split {
                assert_eq!(window.tab_states[0].active_pane, original);
                assert!(window.panes.contains_key(&original));
            }
            let after = window.redraw.snapshot();
            app.handle_pane_process_exited(exiting, sonicterm_types::PaneExitStatus::Code(0));
            assert_eq!(
                app.windows[&owner].redraw.snapshot().0,
                after.0,
                "stale exits do not repeat completion"
            );
            assert!(app.windows[&other].redraw.parked);
            assert_eq!(app.windows[&other].redraw.snapshot().0, other_before.0);
        }
    }
}

/// Both public attach roles and transactional transfer unpark their destination while preserving pane publication identity.
#[test]
fn parked_attach_and_transfer_adapters_preserve_real_pane_identity_and_unpark() {
    for into_main in [false, true] {
        for direct_attach in [false, true] {
            let (mut app, main, child) = topology_owners();
            let (source, target) = if into_main { (child, main) } else { (main, child) };
            let pane = app.windows[&source].tab_states[0].active_pane;
            let generation = Arc::clone(&app.windows[&source].panes[&pane].output_generation);
            generation.fetch_add(3, Ordering::Release);
            let source_before = park_owner(&mut app, source);
            let target_before = park_owner(&mut app, target);
            if direct_attach {
                let (tab, state, panes) = app.detach_from_child(source, 0).unwrap();
                if into_main {
                    app.attach_tab_state(1, tab, state, panes).unwrap();
                } else {
                    app.attach_to_child(target, 1, tab, state, panes).unwrap();
                }
            } else {
                app.transfer_tab(Some(source), 0, Some(target), 1).unwrap();
            }
            let destination = &app.windows[&target];
            assert!(!destination.redraw.parked);
            assert_eq!(
                destination.redraw.snapshot().0[RedrawCause::Topology as usize],
                target_before + 1
            );
            assert_eq!(destination.tab_states[1].active_pane, pane);
            assert!(Arc::ptr_eq(&destination.panes[&pane].output_generation, &generation));
            assert_eq!(generation.load(Ordering::Acquire), 3);
            assert_eq!(destination.panes[&pane].observed_output_generation, 0);
            assert_eq!(*destination.panes[&pane].redraw_target.lock(), Some(target));
            assert!(!app.windows[&source].panes.contains_key(&pane));
            if !direct_attach {
                assert!(!app.windows[&source].redraw.parked);
                assert_eq!(
                    app.windows[&source].redraw.snapshot().0[RedrawCause::Topology as usize],
                    source_before + 1
                );
            }
        }
    }
}

/// A cause is not recovery proof; same-generation and unusable replacement snapshots cannot admit a frame.
#[test]
fn device_recovery_rejects_unbound_causes_and_nonreplacement_snapshots() {
    for child_owner in [false, true] {
        let (mut app, main, child) = owners();
        let owner = if child_owner { child } else { main };
        let stopped = sonicterm_gpu::device_errors::DeviceErrorState::new().snapshot();
        let replacement = sonicterm_gpu::device_errors::DeviceErrorState::new().snapshot();
        let window = app.windows.get_mut(&owner).unwrap();
        window.redraw.stopped_generation = Some(stopped.generation);
        window.redraw.parked = true;
        app.request_owner_redraw(owner, RedrawCause::DeviceRecovered);
        assert_eq!(app.windows[&owner].redraw.stopped_generation, Some(stopped.generation));
        assert!(!app.windows[&owner].redraw.request_in_flight);
        assert!(
            !app.request_recovered_window(owner),
            "an absent renderer cannot validate recovery"
        );
        for (generation, state, destroy_requested) in [
            (stopped.generation, DeviceState::Usable, false),
            (replacement.generation, DeviceState::Unusable, false),
            (replacement.generation, DeviceState::Lost, true),
            (replacement.generation, DeviceState::Usable, true),
        ] {
            let mut rejected = replacement.clone();
            rejected.generation = generation;
            rejected.state = state;
            rejected.destroy_requested = destroy_requested;
            let window = app.windows.get_mut(&owner).unwrap();
            window.redraw.parked = true;
            let before = window.redraw.snapshot();
            assert!(!window.request_device_recovery(&rejected));
            assert_eq!(window.redraw.stopped_generation, Some(stopped.generation));
            assert!(window.redraw.parked);
            assert!(!window.redraw.request_in_flight);
            assert_eq!(window.redraw.snapshot().0, before.0);
        }
        assert!(app.frame_due_work().is_empty());
    }
}

/// Validated replacement state clears suppression exactly once, dirties panes, and coalesces only visible requests.
#[test]
fn usable_different_generation_recovers_once_without_hidden_or_duplicate_native_requests() {
    for child_owner in [false, true] {
        for hidden in [false, true] {
            for in_flight in [false, true] {
                let (mut app, main, child) = owners();
                let owner = if child_owner { child } else { main };
                let stopped = sonicterm_gpu::device_errors::DeviceErrorState::new().snapshot();
                let replacement = sonicterm_gpu::device_errors::DeviceErrorState::new().snapshot();
                assert_ne!(stopped.generation, replacement.generation);
                let window = app.windows.get_mut(&owner).unwrap();
                window.hidden = hidden;
                window.redraw.stopped_generation = Some(stopped.generation);
                window.redraw.parked = true;
                window.redraw.request_in_flight = in_flight;
                for pane in window.panes.values() {
                    pane.parser.lock().grid_mut().clear_dirty();
                }
                let before = window.redraw.snapshot();
                assert!(window.request_device_recovery(&replacement));
                assert_eq!(window.redraw.stopped_generation, None);
                assert!(!window.redraw.parked);
                assert_eq!(window.redraw.request_in_flight, in_flight || !hidden);
                assert_eq!(
                    window.redraw.snapshot().0[RedrawCause::DeviceRecovered as usize],
                    before.0[RedrawCause::DeviceRecovered as usize] + 1
                );
                assert!(window
                    .panes
                    .values()
                    .all(|pane| pane.parser.lock().grid().dirty_count() > 0));
                let after = window.redraw.snapshot();
                assert!(!window.request_device_recovery(&replacement));
                assert_eq!(window.redraw.snapshot().0, after.0);
            }
        }
    }
}

/// The production recovery adapter reads the installed renderer; the pure snapshot seam never supplies its own proof.
#[test]
fn recovery_and_due_service_native_requests_are_validated_and_coalesced_at_the_call_site() {
    let source = concat!(
        include_str!("redraw.rs"),
        "\n",
        include_str!("redraw_admission.rs"),
        "\n",
        include_str!("redraw_state.rs"),
        "\n",
        include_str!("redraw_window.rs")
    );
    let recovery = &source[source.find("pub(super) fn request_recovered_window(").unwrap()
        ..source.find("fn begin_window_redraw(").unwrap()];
    assert!(recovery
        .contains("window.renderer.as_ref().map(|renderer| renderer.device_error_snapshot())"));
    assert!(recovery.contains("window.request_device_recovery(&snapshot)"));
    let request = &source[source.find("fn request_device_recovery(").unwrap()
        ..source.find("fn refresh_monitor_period(").unwrap()];
    assert!(
        request.find("accept_device_recovery(snapshot)").unwrap()
            < request.find("self.request_window_redraw()").unwrap()
    );
    assert!(request.contains("self.frame_deadlines_allowed() && !self.redraw.request_in_flight"));
    assert_eq!(request.matches("self.request_window_redraw()").count(), 1);
    let service = &source[source.find("pub(super) fn service_redraw_due(").unwrap()..];
    assert!(
        service.contains("window.frame_deadlines_allowed() && !window.redraw.request_in_flight")
    );
    assert_eq!(service.matches("window.request_window_redraw()").count(), 1);
}

/// Native visibility changes preserve frame identities and issue exactly one visibility invalidation for either role.
#[test]
fn native_occlusion_transitions_preserve_dirt_and_ignore_duplicate_or_stale_events() {
    for child_owner in [false, true] {
        let (mut app, main, child) = owners();
        let (owner, other) = if child_owner { (child, main) } else { (main, child) };
        let now = Instant::now();
        let pane = app.windows[&owner].tab_states[0].active_pane;
        app.windows[&owner].panes[&pane].parser.lock().grid_mut().mark_all_dirty();
        app.windows[&owner].panes[&pane].output_generation.fetch_add(7, Ordering::Release);
        app.mark_window_redraw(owner, RedrawCause::Input);
        let snapshot = app.windows[&owner].capture_redraw_snapshot();
        let other_before = app.windows[&other].redraw.snapshot();
        let last_render = app.windows[&owner].last_render;
        let stream_clock = app.windows[&owner].stream_clock;
        app.windows.get_mut(&owner).unwrap().retry_not_before = Some(now + Duration::from_secs(2));
        app.windows.get_mut(&owner).unwrap().redraw.deferred = true;
        app.redraw_due = vec![
            DueWork { owner: Some(owner), cause: DueCause::Frame, deadline: now },
            DueWork {
                owner: Some(other),
                cause: DueCause::Frame,
                deadline: now + Duration::from_secs(3),
            },
        ];
        app.handle_window_occlusion(owner, true);
        app.handle_window_occlusion(owner, true);
        assert!(app.windows[&owner].redraw.native_occluded);
        assert!(
            !app.windows[&owner].hidden,
            "native occlusion never substitutes for app hidden state"
        );
        assert_eq!(app.windows[&owner].redraw.snapshot().0, snapshot.causes.0);
        assert_eq!(app.windows[&owner].last_render, last_render);
        assert_eq!(app.windows[&owner].stream_clock, stream_clock);
        assert_eq!(app.windows[&owner].retry_not_before, Some(now + Duration::from_secs(2)));
        assert_eq!(app.windows[&owner].panes[&pane].observed_output_generation, 0);
        assert!(app.windows[&owner].panes[&pane].parser.lock().grid().dirty_count() > 0);
        assert_eq!(app.redraw_due.len(), 1);
        assert_eq!(app.redraw_due[0].owner, Some(other));
        app.handle_window_occlusion(owner, false);
        let visible = app.windows[&owner].redraw.snapshot();
        assert_eq!(
            visible.0[RedrawCause::Visibility as usize],
            snapshot.causes.0[RedrawCause::Visibility as usize] + 1
        );
        app.handle_window_occlusion(owner, false);
        app.handle_window_occlusion(WindowId::from(987654321), false);
        assert_eq!(app.windows[&owner].redraw.snapshot().0, visible.0);
        assert_eq!(app.windows[&other].redraw.snapshot().0, other_before.0);
        assert!(
            !app.windows[&owner].redraw.request_in_flight,
            "no renderer means no usable native target"
        );
    }
}

/// Occluded owner gates run before clocks/locks, suppress every frame contributor, and keep output maintenance alive.
#[test]
fn occluded_real_owner_gate_invokes_no_collector_or_frame_deadlines_but_drains_commands() {
    use crate::app::scrollbar_visibility::ScrollbarVisState;
    for child_owner in [false, true] {
        let (mut app, main, child) = owners();
        let owner = if child_owner { child } else { main };
        let now = Instant::now();
        app.mark_window_redraw(owner, RedrawCause::Input);
        assert_eq!(
            gated_collector_calls(&mut app, owner, now),
            1,
            "same-owner visible baseline reaches the real collector"
        );
        app.software_render_degrade = true;
        app.config.appearance.scrollbar = sonicterm_cfg::config::ScrollbarMode::Auto;
        let window = app.windows.get_mut(&owner).unwrap();
        let pane = window.tab_states[0].active_pane;
        window.redraw.mark(RedrawCause::Input);
        window.last_render = now;
        window.stream_clock = now;
        window.retry_not_before = Some(now + Duration::from_secs(5));
        window.notification = Some(NotificationBubble {
            level: NotificationLevel::Info,
            message: "expires".into(),
            expires_at: Some(now + Duration::from_millis(10)),
        });
        let mut vis = ScrollbarVisState::new(now);
        crate::app::scrollbar_visibility::note_activity(&mut vis, now);
        crate::app::scrollbar_visibility::retarget(
            &mut vis,
            sonicterm_cfg::config::ScrollbarMode::Auto,
            false,
            now,
        );
        vis.alpha = 1.0;
        window.scrollbar_vis.insert(pane, vis);
        window.panes[&pane].command_events.lock().push(crate::app::PaneCommandEvent {
            event: sonicterm_vt::vt::CommandEvent::CmdEnd(Some(0)),
            at: now,
            duration: None,
        });
        app.handle_window_occlusion(owner, true);
        let pending_main = app.pending_redraw;
        let pending_children = app.pending_redraw_windows.clone();
        let mut calls = 0;
        // Every probe is after the contention floor, so only occlusion can prevent collection.
        for tick in 6..=8 {
            app.output_redraw_notification(owner, now);
            calls += gated_collector_calls(&mut app, owner, now + Duration::from_secs(tick));
        }
        assert_eq!(calls, 0);
        let window = &app.windows[&owner];
        assert!(window.panes[&pane].command_events.lock().is_empty());
        assert!(matches!(window.tabs.tabs()[0].command, CommandStatus::Done { .. }));
        assert_eq!(window.last_render, now);
        assert_eq!(window.stream_clock, now);
        assert_eq!(window.retry_not_before, Some(now + Duration::from_secs(5)));
        assert_eq!(app.pending_redraw, pending_main);
        assert_eq!(app.pending_redraw_windows, pending_children);
        assert!(app.frame_due_work().iter().all(|work| work.owner != Some(owner)));
    }
}

/// Timeout owns a paced app deadline; backend occlusion suppresses frames, while atlas/other surface retries retain presenter ownership.
#[test]
fn typed_surface_retry_reasons_keep_distinct_deadline_owners() {
    for child_owner in [false, true] {
        for reason in [
            SurfaceRetryReason::Timeout,
            SurfaceRetryReason::Occluded,
            SurfaceRetryReason::Outdated,
            SurfaceRetryReason::Suboptimal,
            SurfaceRetryReason::SurfaceLost,
        ] {
            let (mut app, main, child) = owners();
            let owner = if child_owner { child } else { main };
            let now = Instant::now();
            app.mark_window_redraw(owner, RedrawCause::Input);
            let snapshot = app.snapshot_window_redraw(owner).unwrap();
            let outcome = PresentOutcome::SurfaceRetry(reason);
            assert_eq!(FrameSettlement::of(&outcome), FrameSettlement::SurfaceRetry(reason));
            app.finish_window_redraw(owner, &snapshot, FrameSettlement::of(&outcome), now);
            let due: Vec<_> =
                app.frame_due_work().into_iter().filter(|work| work.owner == Some(owner)).collect();
            if reason == SurfaceRetryReason::Timeout {
                let period = app.windows[&owner].redraw.monitor_period;
                assert_eq!(due.len(), 1);
                assert_eq!((due[0].cause, due[0].deadline), (DueCause::Frame, now + period));
                app.mark_window_redraw(owner, RedrawCause::Input);
                assert!(
                    !app.begin_window_redraw(owner, now + Duration::from_nanos(1)),
                    "input cannot spin a timed-out surface"
                );
                assert_eq!(app.frame_due_work()[0].deadline, now + period);
            } else if reason == SurfaceRetryReason::Occluded {
                assert!(app.windows[&owner].redraw.backend_occluded);
                assert_eq!(gated_collector_calls(&mut app, owner, now + Duration::from_secs(2)), 0);
                #[cfg(target_os = "macos")]
                {
                    assert_eq!(due.len(), 1);
                    assert_eq!(
                        (due[0].cause, due[0].deadline),
                        (DueCause::SurfaceProbe, now + Duration::from_secs(1))
                    );
                }
                #[cfg(not(target_os = "macos"))]
                assert!(due.is_empty());
            } else {
                assert!(due.is_empty(), "presenter-owned retries add no app timer");
            }
            assert!(
                !app.windows[&owner].redraw.input_pending()
                    || reason == SurfaceRetryReason::Timeout
            );
        }
        let (mut app, main, child) = owners();
        let owner = if child_owner { child } else { main };
        let snapshot = app.snapshot_window_redraw(owner).unwrap();
        app.finish_window_redraw(
            owner,
            &snapshot,
            FrameSettlement::of(&PresentOutcome::AtlasRetry),
            Instant::now(),
        );
        assert!(app.frame_due_work().is_empty(), "atlas retry keeps its native owner");
    }
}

/// Native visibility cannot clear a stopped generation or schedule hidden-main frames; duplicate false preserves unrelated timers.
#[test]
fn hidden_or_stopped_visibility_never_resumes_and_duplicate_false_keeps_existing_work() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    app.windows.get_mut(&main).unwrap().hidden = true;
    app.windows.get_mut(&child).unwrap().redraw.stopped_generation = Some(99);
    for owner in [main, child] {
        app.handle_window_occlusion(owner, true);
        app.handle_window_occlusion(owner, false);
        assert!(!app.windows[&owner].redraw.request_in_flight);
        assert_eq!(gated_collector_calls(&mut app, owner, now), 0);
    }
    assert_eq!(app.windows[&child].redraw.stopped_generation, Some(99));
    app.redraw_due =
        vec![DueWork { owner: Some(child), cause: DueCause::Notification, deadline: now }];
    app.handle_window_occlusion(child, false);
    assert_eq!(app.redraw_due.len(), 1);
}

/// A native event cancels only its own slow probe; exact due identities reject duplicates and later-owner service.
#[cfg(target_os = "macos")]
#[test]
fn backend_probe_deadlines_are_cancelled_by_native_hidden_or_stopped_state_and_settle_once() {
    use sonicterm_gpu::core::SurfaceAvailability;
    let (mut app, main, child) = owners();
    let now = Instant::now();
    for (owner, at) in [(main, now), (child, now + Duration::from_millis(200))] {
        let snapshot = app.snapshot_window_redraw(owner).unwrap();
        app.finish_window_redraw(
            owner,
            &snapshot,
            FrameSettlement::SurfaceRetry(SurfaceRetryReason::Occluded),
            at,
        );
    }
    let first = now + SURFACE_PROBE_PERIOD;
    let later = first + Duration::from_millis(200);
    assert_eq!(app.windows[&main].surface_probe_deadline(), Some(first));
    assert_eq!(app.windows[&child].surface_probe_deadline(), Some(later));
    app.service_surface_probe(child, later, first);
    assert_eq!(app.windows[&child].surface_probe_deadline(), Some(later));
    let main_window = app.windows.get_mut(&main).unwrap();
    main_window.finish_surface_probe(Ok(SurfaceAvailability::Retry), true, first);
    assert_eq!(main_window.surface_probe_deadline(), Some(first + SURFACE_PROBE_PERIOD));
    let before = main_window.redraw.snapshot();
    main_window.finish_surface_probe(
        Ok(SurfaceAvailability::Available),
        true,
        first + SURFACE_PROBE_PERIOD,
    );
    assert_eq!(main_window.surface_probe_deadline(), None);
    assert!(!main_window.redraw.backend_occluded);
    assert_eq!(
        main_window.redraw.snapshot().0[RedrawCause::Visibility as usize],
        before.0[RedrawCause::Visibility as usize] + 1
    );
    let after = main_window.redraw.snapshot();
    app.service_surface_probe(main, first, first + SURFACE_PROBE_PERIOD);
    assert_eq!(app.windows[&main].redraw.snapshot().0, after.0);
    app.redraw_due = app.frame_due_work();
    app.handle_window_occlusion(child, true);
    assert_eq!(app.windows[&child].redraw.surface_probe_at, None);
    assert!(app.redraw_due.iter().all(|work| work.owner != Some(child)));
    let snapshot = app.snapshot_window_redraw(main).unwrap();
    app.finish_window_redraw(
        main,
        &snapshot,
        FrameSettlement::SurfaceRetry(SurfaceRetryReason::Occluded),
        now,
    );
    app.hide_main_window();
    assert_eq!(app.windows[&main].redraw.surface_probe_at, None);
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(main)));
    app.windows.get_mut(&child).unwrap().redraw.native_occluded = false;
    let snapshot = app.snapshot_window_redraw(child).unwrap();
    app.finish_window_redraw(
        child,
        &snapshot,
        FrameSettlement::SurfaceRetry(SurfaceRetryReason::Occluded),
        now,
    );
    let snapshot = app.snapshot_window_redraw(child).unwrap();
    app.finish_window_redraw(child, &snapshot, FrameSettlement::Stopped(5), now);
    assert_eq!(app.windows[&child].redraw.surface_probe_at, None);
}

/// The writers no headless fixture reaches still invalidate at the writer: `begin_window_redraw`'s
/// device-stop branch calls `invalidate_link_pacing`, and every software-degrade writer goes through
/// the invalidating setter instead of assigning the flag.
#[test]
fn device_stop_and_degrade_writers_invalidate_link_pacing() {
    let redraw = concat!(
        include_str!("redraw.rs"),
        "\n",
        include_str!("redraw_admission.rs"),
        "\n",
        include_str!("redraw_state.rs"),
        "\n",
        include_str!("redraw_window.rs")
    )
    .replace("\r\n", "\n");
    let begin = &redraw[redraw.find("fn begin_window_redraw(").unwrap()..];
    let stop = begin.find("if !renderer.device_accepts_gpu_work() {").unwrap();
    let stop_branch = &begin[stop..stop + begin[stop..].find("return false;").unwrap()];
    assert!(stop_branch.contains("invalidate_link_pacing()"), "{stop_branch}");
    for (name, source) in [
        ("config_apply.rs", include_str!("config_apply.rs")),
        ("event_loop.rs", include_str!("event_loop.rs")),
        ("gpu_recovery.rs", include_str!("gpu_recovery.rs")),
    ] {
        assert!(source.contains("set_software_render_degrade("), "{name} uses the setter");
        assert!(!source.contains("self.software_render_degrade ="), "{name} assigns the flag");
    }
}
