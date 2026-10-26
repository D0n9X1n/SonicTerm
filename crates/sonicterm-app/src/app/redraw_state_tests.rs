//! Redraw owner generations, settlement and clocks.

use super::super::redraw_tests::*;
use super::*;

/// Foreground maintenance repaints only changed warning chrome; sampled but unchanged peers remain idle.
#[cfg(windows)]
#[test]
fn foreground_maintenance_requests_only_changed_owners() {
    for changed_role in [None, Some(false), Some(true)] {
        let (mut app, main, child) = owners();
        let start = Instant::now();
        let due = start + super::super::super::FOREGROUND_PROCESS_TTL;
        let changed = changed_role.map(|is_child| if is_child { child } else { main });
        for window in app.windows.values_mut() {
            for pane in window.panes.values_mut() {
                pane.fg_proc_cache = Some((start, None));
            }
        }
        if let Some(id) = changed {
            let window = app.windows.get_mut(&id).unwrap();
            let active = window.tabs.active_index();
            let pane = window.tab_states[active].active_pane;
            window.panes.get_mut(&pane).unwrap().fg_proc_cache = Some((
                start,
                Some(sonicterm_io::proc_info::ForegroundProcess {
                    name: "gsudo".into(),
                    privileged: true,
                }),
            ));
            assert!(window.tabs.set_foreground_privileged(active, true));
        }
        app.foreground_schedule.activity_wake =
            Some(super::super::super::fg_probe::PendingForegroundProbe { due, fixed: true });
        app.redraw_due = vec![DueWork { owner: None, cause: DueCause::Foreground, deadline: due }];
        let before = [main, child].map(|id| app.windows[&id].redraw.snapshot());
        app.service_redraw_due(due - Duration::from_nanos(1));
        assert!(app.windows.values().all(|window| !window.redraw.request_in_flight));
        assert_eq!(app.foreground_schedule.next_deadline(), Some(due));
        app.service_redraw_due(due);
        for (id, baseline) in [main, child].into_iter().zip(before) {
            let window = &app.windows[&id];
            assert_eq!(window.redraw.request_in_flight, Some(id) == changed);
            if Some(id) == changed {
                assert_ne!(window.redraw.snapshot().0, baseline.0);
                assert!(window.redraw.has_pending());
            } else {
                assert_eq!(window.redraw.snapshot().0, baseline.0);
                assert!(!window.redraw.has_pending());
            }
            assert!(window.tabs.tabs().iter().all(|tab| !tab.foreground_privileged));
        }
        assert!(app.redraw_due.is_empty());
        assert_eq!(app.foreground_schedule.next_deadline(), None);
    }
}

/// A silent child command must arm one owner-addressed wake without a prior redraw or input event.
#[test]
fn command_badge_deadline_arms_for_an_idle_child_without_waking_main() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    let until = now + Duration::from_secs(3);
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Done { exit: Some(0), until });
    assert!(
        app.frame_due_work()
            .iter()
            .any(|work| { work.owner == Some(child) && work.deadline == until }),
        "a done badge in an otherwise idle child must contribute its expiry"
    );
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(main)));
    assert!(!app.windows[&main].redraw.has_pending());
}

/// Running badges first appear at six seconds only when their tab is inactive.
#[test]
fn command_badge_running_deadline_follows_active_tab_identity() {
    let (mut app, main, child) = owners();
    let started = Instant::now();
    let until = started + Duration::from_secs(6);
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Running(started));
    assert!(
        app.frame_due_work()
            .iter()
            .any(|work| { work.owner == Some(child) && work.deadline == until }),
        "a silent inactive running command needs a six-second wake"
    );
    app.windows.get_mut(&child).unwrap().tabs.activate(1);
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(child)));
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    assert!(app
        .frame_due_work()
        .iter()
        .any(|work| { work.owner == Some(child) && work.deadline == until }));
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(main)));
}

/// An idle-expiry wake clears the completed status, marks only its owner, and never re-arms itself.
#[test]
fn command_badge_idle_expiry_redraws_its_child_once() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    let until = now + Duration::from_secs(3);
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Done { exit: Some(0), until });
    app.redraw_due = app.frame_due_work();
    assert!(app.redraw_due.iter().any(|work| work.owner == Some(child)));
    app.service_redraw_due(until - Duration::from_nanos(1));
    assert!(!app.windows[&child].redraw.request_in_flight);
    app.service_redraw_due(until);
    assert_eq!(app.windows[&child].tabs.tabs()[1].command, CommandStatus::Idle);
    assert!(app.windows[&child].redraw.request_in_flight);
    assert!(app.windows[&child].redraw.has_pending());
    assert!(!app.windows[&main].redraw.has_pending());
    let after = app.windows[&child].redraw.snapshot();
    app.service_redraw_due(until + Duration::from_nanos(1));
    assert_eq!(app.windows[&child].redraw.snapshot().0, after.0);
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(child)));
}

/// Newer command status invalidates an old due entry instead of expiring or repainting the replacement.
#[test]
fn command_badge_replacement_cannot_spend_an_old_deadline() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    let old_until = now + Duration::from_secs(3);
    let new_until = now + Duration::from_secs(30);
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Done { exit: Some(0), until: old_until });
    app.redraw_due = app.frame_due_work();
    assert!(app.redraw_due.iter().any(|work| work.owner == Some(child)));
    let replacement = CommandStatus::Done { exit: Some(1), until: new_until };
    app.windows.get_mut(&child).unwrap().tabs.set_command_status(1, replacement.clone());
    app.service_redraw_due(old_until);
    assert_eq!(app.windows[&child].tabs.tabs()[1].command, replacement);
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert!(!app.windows[&main].redraw.request_in_flight);
    assert!(app
        .frame_due_work()
        .iter()
        .any(|work| { work.owner == Some(child) && work.deadline == new_until }));
}

/// A hidden tab bar contributes no badge-only wake, even while the terminal itself remains renderable.
#[test]
fn command_badge_hidden_tab_bar_and_suppressed_owner_have_no_frame_wake() {
    let (mut app, _, child) = owners();
    let now = Instant::now();
    app.windows.get_mut(&child).unwrap().tabs.set_command_status(
        1,
        CommandStatus::Done { exit: Some(0), until: now + Duration::from_secs(3) },
    );
    app.tab_bar_visible = false;
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(child)));
    app.tab_bar_visible = true;
    assert!(app.frame_due_work().iter().any(|work| work.owner == Some(child)));
    app.windows.get_mut(&child).unwrap().hidden = true;
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(child)));
    app.windows.get_mut(&child).unwrap().hidden = false;
    app.windows.get_mut(&child).unwrap().redraw.parked = true;
    assert!(app.frame_due_work().iter().all(|work| work.owner != Some(child)));
}

/// A tab closed before its due instant cannot repaint the replacement occupying its old index.
#[test]
fn command_badge_closed_tab_drops_its_captured_due_identity() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    let until = now + Duration::from_secs(3);
    let tab = app.windows[&child].tabs.tabs()[1].id;
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Done { exit: Some(0), until });
    app.redraw_due = app.frame_due_work();
    assert!(app.redraw_due.iter().any(|work| work.owner == Some(child)));
    app.windows.get_mut(&child).unwrap().tabs.close(tab);
    app.windows.get_mut(&child).unwrap().tabs.push(sonicterm_ui::tabs::Tab::new("replacement"));
    app.service_redraw_due(until);
    assert_eq!(app.windows[&child].tabs.tabs()[1].command, CommandStatus::Idle);
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert!(!app.windows[&child].redraw.has_pending());
    assert!(!app.windows[&main].redraw.has_pending());
    assert!(app.frame_due_work().is_empty());
}

/// Becoming active after arming Running cancels that badge without spending a redraw generation.
#[test]
fn command_badge_running_due_is_invalidated_by_activation() {
    let (mut app, main, child) = owners();
    let started = Instant::now();
    let until = started + Duration::from_secs(6);
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Running(started));
    app.redraw_due = app.frame_due_work();
    assert!(app.redraw_due.iter().any(|work| work.owner == Some(child)));
    app.windows.get_mut(&child).unwrap().tabs.activate(1);
    app.service_redraw_due(until);
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert!(!app.windows[&child].redraw.has_pending());
    assert!(!app.windows[&main].redraw.has_pending());
    assert!(app.frame_due_work().is_empty());
    assert_eq!(app.windows[&child].tabs.tabs()[1].command, CommandStatus::Running(started));
}

/// Suppression after a timer was armed retains changed chrome without requesting a hidden frame.
#[test]
fn command_badge_due_while_hidden_retains_only_owner_dirt() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    let until = now + Duration::from_secs(3);
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Done { exit: Some(0), until });
    app.redraw_due = app.frame_due_work();
    assert!(app.redraw_due.iter().any(|work| work.owner == Some(child)));
    app.windows.get_mut(&child).unwrap().hidden = true;
    app.service_redraw_due(until);
    assert_eq!(app.windows[&child].tabs.tabs()[1].command, CommandStatus::Idle);
    assert!(app.windows[&child].redraw.has_pending());
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert!(!app.windows[&main].redraw.has_pending());
    assert!(app.frame_due_work().is_empty());
}

/// Running and Done transitions fire exactly at their boundaries and cannot become a periodic wake.
#[test]
fn command_badge_fake_clock_boundaries_expire_once_for_both_owner_roles() {
    for child_owner in [false, true] {
        for running in [false, true] {
            for active in [false, true] {
                let (mut app, main, child) = topology_owners();
                let (owner, peer) = if child_owner { (child, main) } else { (main, child) };
                let now = Instant::now();
                let due = now + Duration::from_secs(if running { 6 } else { 3 });
                let status = if running {
                    CommandStatus::Running(now)
                } else {
                    CommandStatus::Done { exit: Some(0), until: due }
                };
                let window = app.windows.get_mut(&owner).unwrap();
                window.tabs.activate(usize::from(active));
                window.tabs.set_command_status(1, status.clone());
                let due_work = app.frame_due_work_at(now);
                let badge_due: Vec<_> = due_work
                    .iter()
                    .filter(|work| matches!(work.cause, DueCause::CommandBadge { .. }))
                    .collect();
                if running && active {
                    assert!(badge_due.is_empty());
                    continue;
                }
                assert_eq!(badge_due.len(), 1);
                assert_eq!(badge_due[0].owner, Some(owner));
                assert_eq!(badge_due[0].deadline, due);
                app.redraw_due = due_work;
                app.service_redraw_due(due - Duration::from_nanos(1));
                assert!(!app.windows[&owner].redraw.request_in_flight);
                app.service_redraw_due(due);
                assert!(app.windows[&owner].redraw.request_in_flight);
                assert!(!app.windows[&peer].redraw.has_pending());
                let after = app.windows[&owner].redraw.snapshot();
                let painted = app.windows[&owner].tabs.tabs()[1].command.clone().badge(due, active);
                assert_eq!(painted, running.then_some("…"));
                let frame = app.snapshot_window_redraw_at(owner, due).unwrap();
                app.finish_window_redraw(owner, &frame, FrameSettlement::Presented, due);
                assert!(app.frame_due_work_at(due).is_empty());
                assert!(app.frame_due_work_at(due + Duration::from_nanos(1)).is_empty());
                app.service_redraw_due(due + Duration::from_secs(30));
                assert_eq!(app.windows[&owner].redraw.snapshot().0, after.0);
                assert!(!app.windows[&owner].redraw.request_in_flight);
                assert!(!app.windows[&peer].redraw.has_pending());
            }
        }
    }
}

/// A transferred badge discards its old owner token and expires only in its actual new window.
#[test]
fn command_badge_transfer_recomputes_owner_without_reusing_the_source_deadline() {
    for into_main in [false, true] {
        let (mut app, main, child) = topology_owners();
        let (source, target) = if into_main { (child, main) } else { (main, child) };
        let now = Instant::now();
        let due = now + Duration::from_secs(3);
        let moved_id = app.windows[&source].tabs.tabs()[1].id;
        let status = CommandStatus::Done { exit: Some(0), until: due };
        app.windows.get_mut(&source).unwrap().tabs.set_command_status(1, status.clone());
        app.windows.get_mut(&source).unwrap().tab_states[1].command = status;
        app.redraw_due = app.frame_due_work_at(now);
        assert_eq!(app.redraw_due.len(), 1);
        assert_eq!(app.redraw_due[0].owner, Some(source));
        app.transfer_tab(Some(source), 1, Some(target), 1).unwrap();
        assert_eq!(app.windows[&target].tabs.tabs()[1].id, moved_id);
        let source_before = app.windows[&source].redraw.snapshot();
        let target_before = app.windows[&target].redraw.snapshot();
        let current = app.frame_due_work_at(now);
        let target_due: Vec<_> = current
            .iter()
            .filter(|work| matches!(work.cause, DueCause::CommandBadge { .. }))
            .collect();
        assert_eq!(target_due.len(), 1);
        assert_eq!(target_due[0].owner, Some(target));
        app.service_redraw_due(due);
        assert_eq!(app.windows[&source].redraw.snapshot().0, source_before.0);
        assert_eq!(app.windows[&target].redraw.snapshot().0, target_before.0);
        // Replay only the newly folded target token after proving the old source token is inert.
        app.redraw_due = target_due.into_iter().copied().collect();
        app.service_redraw_due(due);
        assert_eq!(app.windows[&target].tabs.tabs()[1].command, CommandStatus::Idle);
        assert_eq!(
            app.windows[&target].redraw.snapshot().0[RedrawCause::Chrome as usize],
            target_before.0[RedrawCause::Chrome as usize] + 1
        );
        assert_eq!(app.windows[&source].redraw.snapshot().0, source_before.0);
        assert!(app
            .frame_due_work_at(due)
            .iter()
            .all(|work| { !matches!(work.cause, DueCause::CommandBadge { .. }) }));
    }
}

/// Coincident badges share one native request, while a future sibling and maintenance remain separate.
#[test]
fn command_badge_coalesces_current_owner_and_preserves_later_work() {
    let (mut app, main, child) = topology_owners();
    let now = Instant::now();
    let due = now + Duration::from_secs(3);
    for index in 0..2 {
        app.windows
            .get_mut(&child)
            .unwrap()
            .tabs
            .set_command_status(index, CommandStatus::Done { exit: Some(0), until: due });
    }
    app.windows.get_mut(&main).unwrap().tabs.set_command_status(
        0,
        CommandStatus::Done { exit: Some(1), until: due + Duration::from_secs(30) },
    );
    app.windows.get_mut(&child).unwrap().redraw.request_in_flight = true;
    app.redraw_due = app.frame_due_work_at(now);
    app.redraw_due.push(DueWork { owner: None, cause: DueCause::Memory, deadline: due });
    app.service_redraw_due(due);
    assert!(app.windows[&child].redraw.request_in_flight);
    assert!(app.windows[&child].tabs.tabs().iter().all(|tab| tab.command == CommandStatus::Idle));
    assert!(!app.windows[&main].redraw.has_pending());
    assert_eq!(app.redraw_due.len(), 1);
    assert_eq!(app.redraw_due[0].owner, Some(main));
    let settled = app.windows[&child].redraw.snapshot();
    app.service_redraw_due(due);
    assert_eq!(app.windows[&child].redraw.snapshot().0, settled.0);
}

/// An inactive Running badge must not lose a newly eligible transition between painting and folding.
#[test]
fn command_badge_transition_after_newly_eligible_paint_survives_the_fold() {
    let (mut app, main, child) = owners();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(6);
    app.windows.get_mut(&child).unwrap().tabs.activate(1);
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Running(started));
    assert!(app.frame_due_work_at(started).is_empty(), "active Running has no old timer");
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    let painted_at = deadline - Duration::from_millis(1);
    assert_eq!(app.windows[&child].tabs.tabs()[1].command.clone().badge(painted_at, false), None);
    let snapshot = app.snapshot_window_redraw_at(child, painted_at).unwrap();
    app.finish_window_redraw(child, &snapshot, FrameSettlement::Presented, painted_at);
    assert!(!app.windows[&child].redraw.has_pending());
    let fold_at = deadline + Duration::from_millis(1);
    app.redraw_due = app.refresh_frame_due_work_at(fold_at);
    assert!(
        app.windows[&child].redraw.request_in_flight
            || app.redraw_due.iter().any(|work| {
                work.owner == Some(child) && matches!(work.cause, DueCause::CommandBadge { .. })
            }),
        "the newly eligible transition crossed during paint/fold and still requires its owner"
    );
    assert!(!app.windows[&main].redraw.has_pending());
}

/// A restored window can paint Done just before expiry; that newly visible timer must still fire.
#[test]
fn command_badge_done_expiry_after_restored_paint_survives_the_fold() {
    let (mut app, main, child) = owners();
    let now = Instant::now();
    let deadline = now + Duration::from_secs(3);
    app.windows.get_mut(&child).unwrap().hidden = true;
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Done { exit: Some(0), until: deadline });
    assert!(app.frame_due_work_at(now).is_empty(), "hidden owner has no old timer");
    app.windows.get_mut(&child).unwrap().hidden = false;
    let painted_at = deadline - Duration::from_millis(1);
    assert_eq!(
        app.windows[&child].tabs.tabs()[1].command.clone().badge(painted_at, false),
        Some("✓")
    );
    let snapshot = app.snapshot_window_redraw_at(child, painted_at).unwrap();
    app.finish_window_redraw(child, &snapshot, FrameSettlement::Presented, painted_at);
    let fold_at = deadline + Duration::from_millis(1);
    app.redraw_due = app.refresh_frame_due_work_at(fold_at);
    assert!(
        app.windows[&child].redraw.request_in_flight
            || app.redraw_due.iter().any(|work| {
                work.owner == Some(child) && matches!(work.cause, DueCause::CommandBadge { .. })
            }),
        "the just-painted Done badge must not survive indefinitely past its expiry"
    );
    assert!(!app.windows[&main].redraw.has_pending());
}

/// Production captures badge deadlines before both role renders and services them before the next wait.
#[test]
fn command_badge_role_capture_and_wait_fold_preserve_due_order() {
    for (source, command_poll) in [
        (
            concat!(
                include_str!("window_event.rs"),
                "\n",
                include_str!("window_event_pointer.rs"),
                "\n",
                include_str!("window_event_redraw.rs")
            ),
            "self.poll_command_events_for_all_tabs();",
        ),
        (
            include_str!("child_window_redraw.rs"),
            "poll_command_events_for_child_window(child, config);",
        ),
    ] {
        let poll = source.find(command_poll).unwrap();
        let capture = source
            .find("sources.try_collect(|| self.snapshot_window_redraw(win_id))")
            .expect("each production role uses the scheduler snapshot before its parser locks");
        let render = source.find("r.render_releasing(").unwrap();
        assert!(poll < capture && capture < render);
    }
    let source = concat!(include_str!("event_loop.rs"), "\n", include_str!("event_loop_wait.rs"));
    let about = source.split_once("pub(in crate::app) fn do_about_to_wait(").unwrap().1;
    let fold = about.find("self.refresh_frame_due_work_at(now)").unwrap();
    let arm = about.find("self.redraw_due = due").unwrap();
    assert!(fold < arm, "the real event loop must preserve captured elapsed deadlines");
    let refresh = source.split_once("pub(in crate::app) fn refresh_frame_due_work_at(").unwrap().1;
    assert!(
        refresh.find("self.service_redraw_due(now)").unwrap()
            < refresh.find("self.frame_due_work_at(now)").unwrap()
    );
}

/// Failed and paced retries retain one captured transition without repeatedly waking an already pending frame.
#[test]
fn command_badge_prepaint_capture_is_bounded_across_retry_and_threshold() {
    let (mut app, main, child) = owners();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(6);
    app.windows
        .get_mut(&child)
        .unwrap()
        .tabs
        .set_command_status(1, CommandStatus::Running(started));
    for offset in [3, 2, 1] {
        let at = deadline - Duration::from_millis(offset);
        let frame = app.snapshot_window_redraw_at(child, at).unwrap();
        app.finish_window_redraw(child, &frame, FrameSettlement::Failed, at);
        assert_eq!(app.redraw_due.len(), 1, "retry capture must deduplicate the tab token");
    }
    let fold_at = deadline + Duration::from_nanos(1);
    app.redraw_due = app.refresh_frame_due_work_at(fold_at);
    assert!(app.windows[&child].redraw.request_in_flight);
    let requested = app.windows[&child].redraw.snapshot();
    for offset in 2..5 {
        app.redraw_due = app.refresh_frame_due_work_at(deadline + Duration::from_nanos(offset));
        assert!(app.redraw_due.is_empty(), "no expired deadline may spin the loop");
        assert_eq!(app.windows[&child].redraw.snapshot().0, requested.0);
    }
    assert!(!app.windows[&main].redraw.has_pending());
}

/// Native occlusion cancels captured badge wakes and restores only the current owner's future transition.
#[test]
fn command_badge_native_occlusion_recomputes_both_owner_roles_without_assembly() {
    for child_owner in [false, true] {
        for running in [false, true] {
            let (mut app, main, child) = topology_owners();
            let (owner, peer) = if child_owner { (child, main) } else { (main, child) };
            let now = Instant::now();
            let deadline = now + Duration::from_secs(if running { 6 } else { 3 });
            let status = if running {
                CommandStatus::Running(now)
            } else {
                CommandStatus::Done { exit: Some(0), until: deadline }
            };
            let window = app.windows.get_mut(&owner).unwrap();
            window.tabs.activate(0);
            window.tabs.set_command_status(1, status);
            window.mark_redraw(RedrawCause::Chrome);
            let pending = window.redraw.snapshot();
            let peer_before = app.windows[&peer].redraw.snapshot();
            app.redraw_due = app.frame_due_work_at(now);
            assert_eq!(app.redraw_due.len(), 1);
            app.handle_window_occlusion(owner, true);
            assert!(app.redraw_due.is_empty());
            assert!(app.frame_due_work_at(now).is_empty());
            assert_eq!(gated_collector_calls(&mut app, owner, now), 0);
            assert_eq!(app.windows[&owner].redraw.snapshot().0, pending.0);
            assert!(!app.windows[&owner].redraw.request_in_flight);
            app.handle_window_occlusion(owner, false);
            let restored = app.frame_due_work_at(deadline - Duration::from_nanos(1));
            assert_eq!(restored.len(), 1);
            assert_eq!(restored[0].owner, Some(owner));
            assert_eq!(restored[0].deadline, deadline);
            app.redraw_due = restored;
            app.service_redraw_due(deadline);
            assert!(app.windows[&owner].redraw.request_in_flight);
            assert_eq!(app.windows[&peer].redraw.snapshot().0, peer_before.0);
            let frame = app.snapshot_window_redraw_at(owner, deadline).unwrap();
            app.finish_window_redraw(owner, &frame, FrameSettlement::Presented, deadline);
            assert!(app.frame_due_work_at(deadline).is_empty());
        }
    }
}

/// A captured badge may dirty a backend-occluded owner, but only its bounded surface probe can wake it.
#[test]
fn command_badge_backend_occlusion_retains_chrome_without_a_frame_deadline() {
    for child_owner in [false, true] {
        let (mut app, main, child) = topology_owners();
        let (owner, peer) = if child_owner { (child, main) } else { (main, child) };
        let now = Instant::now();
        let deadline = now + Duration::from_secs(3);
        app.windows
            .get_mut(&owner)
            .unwrap()
            .tabs
            .set_command_status(1, CommandStatus::Done { exit: Some(0), until: deadline });
        let frame = app.snapshot_window_redraw_at(owner, now).unwrap();
        app.finish_window_redraw(
            owner,
            &frame,
            FrameSettlement::SurfaceRetry(SurfaceRetryReason::Occluded),
            now,
        );
        let pending = app.windows[&owner].redraw.snapshot();
        assert_eq!(gated_collector_calls(&mut app, owner, deadline), 0);
        let next = app.frame_due_work_at(now);
        assert!(!next.iter().any(|work| matches!(work.cause, DueCause::CommandBadge { .. })));
        #[cfg(target_os = "macos")]
        {
            assert_eq!(next.len(), 1);
            assert_eq!(next[0].cause, DueCause::SurfaceProbe);
            assert_eq!(next[0].deadline, now + SURFACE_PROBE_PERIOD);
        }
        #[cfg(not(target_os = "macos"))]
        assert!(next.is_empty());
        app.service_redraw_due(deadline);
        assert_eq!(app.windows[&owner].tabs.tabs()[1].command, CommandStatus::Idle);
        assert_eq!(
            app.windows[&owner].redraw.snapshot().0[RedrawCause::Chrome as usize],
            pending.0[RedrawCause::Chrome as usize] + 1
        );
        assert!(!app.windows[&owner].redraw.request_in_flight);
        assert!(!app.windows[&peer].redraw.has_pending());
        assert_eq!(gated_collector_calls(&mut app, owner, deadline), 0);
    }
}

/// Completing either owner cannot consume the other owner's input or output identities.
#[test]
fn two_window_input_output_permutations_settle_only_the_snapshot_owner() {
    for child_first in [false, true] {
        let (mut app, main, child) = owners();
        let now = Instant::now();
        app.mark_window_redraw(main, RedrawCause::Input);
        app.mark_window_redraw(child, RedrawCause::Input);
        for id in [main, child] {
            let pane = app.windows[&id].tab_states[0].active_pane;
            app.windows[&id].panes[&pane].output_generation.fetch_add(1, Ordering::Release);
        }
        let (first, other) = if child_first { (child, main) } else { (main, child) };
        let first_snapshot = app.snapshot_window_redraw(first).unwrap();
        app.finish_window_redraw(first, &first_snapshot, FrameSettlement::Presented, now);
        assert!(!app.windows[&first].redraw.input_pending());
        assert!(app.windows[&other].redraw.input_pending());
        assert!(!app.windows[&first].visible_output_advanced());
        assert!(app.windows[&other].visible_output_advanced());
    }
}

/// An actual collector callback captures generations before locking, not at completion.
#[test]
fn output_and_input_arriving_after_collector_snapshot_remain_pending() {
    let (mut app, main, _) = owners();
    let pane = app.windows[&main].tab_states[0].active_pane;
    let published = Arc::clone(&app.windows[&main].panes[&pane].output_generation);
    app.mark_window_redraw(main, RedrawCause::Input);
    let sources = app
        .main_visible_frame_sources(sonicterm_ui::pane::Rect::new(0.0, 0.0, 100.0, 100.0))
        .ok()
        .unwrap();
    let held = sources.try_collect(|| app.snapshot_window_redraw(main)).ok().unwrap();
    let snapshot = held.snapshot.as_ref().unwrap().clone();
    assert!(app.windows[&main].panes[&pane].parser.try_lock().is_none());
    published.fetch_add(1, Ordering::Release);
    app.mark_window_redraw(main, RedrawCause::Input);
    drop(held);
    drop(sources);
    app.finish_window_redraw(main, &snapshot, FrameSettlement::Cached, Instant::now());
    assert!(app.windows[&main].redraw.input_pending());
    assert!(app.windows[&main].visible_output_advanced());
}

/// Moving the real PaneState keeps its output Arc and observed identity instead of inventing a window counter.
#[test]
fn pane_transfer_preserves_generation_arc_and_unobserved_output() {
    let (mut app, main, child) = owners();
    let pane_id = app.windows[&main].tab_states[0].active_pane;
    let arc = Arc::clone(&app.windows[&main].panes[&pane_id].output_generation);
    arc.fetch_add(3, Ordering::Release);
    let pane = app.windows.get_mut(&main).unwrap().panes.remove(&pane_id).unwrap();
    app.windows.get_mut(&child).unwrap().panes.insert(pane_id, pane);
    let destination = app.windows.get_mut(&child).unwrap();
    destination.tab_states[0].tree = sonicterm_ui::pane::PaneTree::leaf(pane_id);
    destination.tab_states[0].active_pane = pane_id;
    destination.tabs.activate(0);
    assert!(Arc::ptr_eq(&arc, &destination.panes[&pane_id].output_generation));
    assert!(destination.visible_output_advanced());
    let snapshot = destination.capture_redraw_snapshot();
    arc.fetch_add(1, Ordering::Release);
    destination.settle_output_snapshot(&snapshot);
    assert_eq!(destination.panes[&pane_id].observed_output_generation, 3);
    assert!(destination.visible_output_advanced());
}

/// Equal totals across panes are not equal identities, and hidden publication alone never arms another frame.
#[test]
fn pane_generation_vectors_do_not_alias_or_create_hidden_output_heartbeats() {
    let (mut app, _, child) = owners();
    let state = app.windows.get_mut(&child).unwrap();
    state.tabs.activate(0);
    let visible = state.tab_states[0].active_pane;
    let hidden = state.tab_states[1].active_pane;
    state.panes[&hidden].output_generation.fetch_add(2, Ordering::Release);
    let first = state.capture_redraw_snapshot();
    assert!(!state.visible_output_advanced());
    state.settle_output_snapshot(&first);
    state.panes[&visible].output_generation.fetch_add(2, Ordering::Release);
    assert!(state.visible_output_advanced());
    assert_eq!(state.panes[&hidden].observed_output_generation, 2);
    assert!(app.frame_due_work().is_empty());
}

/// Mixed hardware refresh rates wake only their due owner and never repeat a still-pending native request.
#[test]
fn mixed_monitor_deadlines_follow_each_owner() {
    for (main_rate, child_rate) in [(60_000_u64, 120_000_u64), (120_000, 60_000)] {
        let (mut app, main, child) = owners();
        let now = Instant::now();
        let main_period = Duration::from_micros(1_000_000_000 / main_rate);
        let child_period = Duration::from_micros(1_000_000_000 / child_rate);
        let fast_period = Duration::from_micros(8_333);
        let slow_period = Duration::from_micros(16_666);
        assert_eq!(main_period.min(child_period), fast_period);
        assert_eq!(main_period.max(child_period), slow_period);
        app.software_render_degrade = false;
        app.frame_period = main_period;
        app.monitor_frame_period = main_period;
        for (id, period) in [(main, main_period), (child, child_period)] {
            let state = app.windows.get_mut(&id).unwrap();
            state.redraw = WindowRedrawState {
                monitor_period: period,
                deferred: true,
                ..WindowRedrawState::default()
            };
            state.last_render = now;
            state.stream_clock = now;
            state.retry_not_before = None;
        }

        let due = app.frame_due_work();
        assert_eq!(due.len(), 2);
        for (id, period) in [(main, main_period), (child, child_period)] {
            assert!(
                due.iter().any(|work| work.owner == Some(id)
                    && work.cause == DueCause::Frame
                    && work.deadline == now + period),
                "owner {id:?} must keep its own monitor deadline"
            );
        }
        let (fast, slow) = if main_period < child_period { (main, child) } else { (child, main) };
        app.redraw_due = due;
        app.service_redraw_due(now + fast_period - Duration::from_nanos(1));
        for id in [main, child] {
            assert!(!app.windows[&id].redraw.request_in_flight);
            assert_eq!(app.windows[&id].redraw.snapshot().0, [0; CAUSES]);
        }

        app.redraw_due = app.frame_due_work();
        app.service_redraw_due(now + fast_period);
        assert!(app.windows[&fast].redraw.request_in_flight);
        assert!(!app.windows[&slow].redraw.request_in_flight);
        let fast_causes = app.windows[&fast].redraw.snapshot().0;
        let due = app.frame_due_work();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].owner, Some(slow));
        assert_eq!(due[0].cause, DueCause::Frame);
        assert_eq!(due[0].deadline, now + slow_period);
        app.redraw_due = due;
        app.service_redraw_due(now + fast_period);
        assert_eq!(app.windows[&fast].redraw.snapshot().0, fast_causes);
        assert!(!app.windows[&slow].redraw.request_in_flight);

        app.redraw_due = app.frame_due_work();
        app.service_redraw_due(now + slow_period);
        let mut expected_causes = [0; CAUSES];
        expected_causes[RedrawCause::Expose as usize] = 1;
        for id in [main, child] {
            let state = &app.windows[&id];
            assert!(state.redraw.request_in_flight);
            assert_eq!(state.redraw.snapshot().0, expected_causes);
            assert_eq!(state.last_render, now);
            assert_eq!(state.stream_clock, now);
            assert_eq!(state.redraw.last_present, None);
        }
        assert!(app.frame_due_work().is_empty());
    }
}

/// The production fold preserves 60/120 Hz owners, exact software/IME periods, and the contention floor.
#[test]
fn owner_monitor_pacing_preserves_hardware_software_ime_and_contention() {
    for period in [Duration::from_micros(16_667), Duration::from_micros(8_333)] {
        for software in [false, true] {
            for composing in [false, true] {
                let (mut app, main, child) = owners();
                app.software_render_degrade = software;
                let now = Instant::now();
                for id in [main, child] {
                    let state = app.windows.get_mut(&id).unwrap();
                    state.redraw.monitor_period = period;
                    state.last_render = now;
                    state.stream_clock = now;
                    state.redraw.deferred = true;
                    if composing {
                        state.ime.handle_preedit("中", None);
                    }
                }
                let effective = crate::app::effective_frame_period(software, composing, period);
                let due = app.frame_due_work();
                assert_eq!(due.iter().filter(|work| work.cause == DueCause::Frame).count(), 2);
                assert!(due.iter().all(|work| work.deadline == now + effective));
                app.mark_window_redraw(main, RedrawCause::Input);
                assert_eq!(
                    app.begin_window_redraw(main, now + Duration::from_micros(1)),
                    !software
                );
                app.defer_window_lock_contention(child, true, now);
                let floor = app.windows[&child].retry_not_before.unwrap();
                app.mark_window_redraw(child, RedrawCause::Input);
                assert!(!app.begin_window_redraw(child, now + Duration::from_micros(1)));
                app.defer_window_lock_contention(child, false, now + Duration::from_micros(2));
                assert_eq!(app.windows[&child].retry_not_before, Some(floor));
            }
        }
    }
}
