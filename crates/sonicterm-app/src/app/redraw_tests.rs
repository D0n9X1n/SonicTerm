use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_ui::{
    overlays::{NotificationBubble, NotificationLevel},
    pane::Rect,
    tabs::CommandStatus,
};
use std::sync::Arc;

/// Real owner state without a native renderer, so fake-clock assertions exercise production adapters.
fn owners() -> (App, WindowId, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child", "background"]);
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    (app, main, child)
}

/// Foreground maintenance repaints only changed warning chrome; sampled but unchanged peers remain idle.
#[cfg(windows)]
#[test]
fn foreground_maintenance_requests_only_changed_owners() {
    for changed_role in [None, Some(false), Some(true)] {
        let (mut app, main, child) = owners();
        let start = Instant::now();
        let due = start + super::super::FOREGROUND_PROCESS_TTL;
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
            Some(super::super::fg_probe::PendingForegroundProbe { due, fixed: true });
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
        (include_str!("window_event.rs"), "self.poll_command_events_for_all_tabs();"),
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
    let source = include_str!("event_loop.rs");
    let about = source.split_once("pub(super) fn do_about_to_wait(").unwrap().1;
    let fold = about.find("self.refresh_frame_due_work_at(now)").unwrap();
    let arm = about.find("self.redraw_due = due").unwrap();
    assert!(fold < arm, "the real event loop must preserve captured elapsed deadlines");
    let refresh = source.split_once("pub(super) fn refresh_frame_due_work_at(").unwrap().1;
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

/// One attempt for `cause` ending in `outcome` moves both clocks and is followed by a paced repeat.
fn assert_paced_repeat(cause: RedrawCause, outcome: FrameSettlement) {
    let (mut app, main, _) = owners();
    let pane = app.windows[&main].tab_states[0].active_pane;
    app.windows[&main].panes[&pane].parser.lock().grid_mut().mark_all_dirty();
    let now = Instant::now();
    app.mark_window_redraw(main, cause);
    let snapshot = app.snapshot_window_redraw(main).unwrap();
    app.finish_window_redraw(main, &snapshot, outcome, now);
    assert_eq!(app.windows[&main].last_render, now);
    assert_eq!(app.windows[&main].stream_clock, now, "{outcome:?} paces streaming from itself");
    assert!(!app.windows[&main].redraw.input_pending());
    assert!(app.windows[&main].panes[&pane].parser.lock().grid().dirty_count() > 0);
    assert!(app.frame_due_work().is_empty(), "dirt or retained causes alone never arm a timer");
    assert!(
        !app.begin_window_redraw(main, now + Duration::from_micros(1)),
        "presenter-owned repeats stay paced ({outcome:?})"
    );
}

/// A settled hardware keypress spends its input without a heartbeat, keeps the streaming clock, and
/// the next non-input completion moves that clock again and restores streaming deferral.
#[test]
fn settled_keypress_keeps_the_streaming_clock_until_the_next_non_input_completion() {
    use super::super::frame_counters::DeferRule;
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
    let source = include_str!("redraw.rs");
    let entry = source.find("pub(super) fn begin_window_redraw(").unwrap();
    let report = source[entry..].find("take_stopped_render_outcome()").unwrap();
    let parked = source[entry..].find("!window.frame_deadlines_allowed()").unwrap();
    assert!(report < parked);
}

/// Real role adapters consume the callback's typed snapshot and inspect outcomes before result conversion.
#[test]
fn production_roles_preserve_prelock_snapshot_and_exact_attempt_accounting() {
    for source in [
        concat!(
            include_str!("window_event.rs"),
            include_str!("window_keyboard.rs"),
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

/// Give both roles real resize-adapter geometry and enough tabs to preserve each owner after a removal.
fn topology_owners() -> (App, WindowId, WindowId) {
    let (mut app, main, child) = owners();
    app.__test_seed_tab("main survivor");
    app.windows.get_mut(&main).unwrap().tabs.activate(0);
    app.__test_set_main_pane_viewport(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
    app.windows.get_mut(&child).unwrap().test_pane_viewport =
        Some((Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0));
    (app, main, child)
}

/// Enter the same parked scheduler state as a rejected collector, without manufacturing invalid pane geometry.
fn park_owner(app: &mut App, id: WindowId) -> u64 {
    let window = app.windows.get_mut(&id).unwrap();
    let snapshot = window.redraw.snapshot();
    window.redraw.park(snapshot);
    window.redraw.request_in_flight = false;
    snapshot.0[RedrawCause::Topology as usize]
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
            app.handle_pane_process_exited(exiting, Some(true));
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
            app.handle_pane_process_exited(exiting, Some(true));
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
    let source = include_str!("redraw.rs");
    let recovery = &source[source.find("pub(super) fn request_recovered_window(").unwrap()
        ..source.find("pub(super) fn begin_window_redraw(").unwrap()];
    assert!(recovery
        .contains("window.renderer.as_ref().map(|renderer| renderer.device_error_snapshot())"));
    assert!(recovery.contains("window.request_device_recovery(&snapshot)"));
    let request = &source[source.find("fn request_device_recovery(").unwrap()
        ..source.find("pub(super) fn refresh_monitor_period(").unwrap()];
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

/// Count the exact production collector adapter invocation behind its real owner gate, without a native event loop.
fn gated_collector_calls(app: &mut App, owner: WindowId, now: Instant) -> usize {
    if !app.begin_window_redraw(owner, now) {
        return 0;
    }
    let rect = Rect::new(0.0, 0.0, 800.0, 480.0);
    let sources = if Some(owner) == app.main_window_id {
        app.main_visible_frame_sources(rect)
    } else {
        app.child_visible_frame_sources(owner, rect)
    };
    let sources = sources.expect("valid seeded owner");
    let _held = sources.try_collect(|| app.snapshot_window_redraw(owner)).ok().unwrap();
    1
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

/// A failed native probe keeps only its owner's slow retry alive while preserving dirt and suppressing assembly.
#[cfg(target_os = "macos")]
#[test]
fn surface_probe_errors_rearm_without_consuming_frame_or_output_identity() {
    for child_owner in [false, true] {
        let (mut app, main, child) = owners();
        let (owner, other) = if child_owner { (child, main) } else { (main, child) };
        let now = Instant::now();
        let pane = app.windows[&owner].tab_states[0].active_pane;
        app.windows[&owner].panes[&pane].parser.lock().grid_mut().mark_all_dirty();
        app.windows[&owner].panes[&pane].output_generation.fetch_add(7, Ordering::Release);
        let snapshot = app.snapshot_window_redraw(owner).unwrap();
        app.finish_window_redraw(
            owner,
            &snapshot,
            FrameSettlement::SurfaceRetry(SurfaceRetryReason::Occluded),
            now,
        );
        let pending = app.windows[&owner].redraw.snapshot();
        let other_before = app.windows[&other].redraw.snapshot();
        for tick in 1..=3 {
            let at = now + SURFACE_PROBE_PERIOD * tick;
            let window = app.windows.get_mut(&owner).unwrap();
            window.finish_surface_probe(Err(anyhow::anyhow!("surface creation failed")), true, at);
            assert_eq!(window.surface_probe_deadline(), Some(at + SURFACE_PROBE_PERIOD));
            assert!(window.redraw.backend_occluded);
            assert!(!window.redraw.request_in_flight);
            assert_eq!(window.redraw.snapshot().0, pending.0);
            assert_eq!(window.last_render, now);
            assert_eq!(window.panes[&pane].observed_output_generation, 0);
            assert!(window.panes[&pane].parser.lock().grid().dirty_count() > 0);
            let due: Vec<_> =
                app.frame_due_work().into_iter().filter(|work| work.owner == Some(owner)).collect();
            assert_eq!(due.len(), 1);
            assert_eq!(
                (due[0].cause, due[0].deadline),
                (DueCause::SurfaceProbe, at + SURFACE_PROBE_PERIOD)
            );
            assert_eq!(gated_collector_calls(&mut app, owner, at), 0);
            assert_eq!(app.windows[&other].redraw.snapshot().0, other_before.0);
        }
    }
}

/// Device refusal wins even before the App records it; hidden, parked, or native-occluded owners cannot rearm.
#[cfg(target_os = "macos")]
#[test]
fn surface_probe_errors_do_not_rearm_suppressed_or_unusable_owners() {
    for mode in 0..5 {
        let (mut app, main, _) = owners();
        let now = Instant::now();
        let window = app.windows.get_mut(&main).unwrap();
        window.redraw.backend_occluded = true;
        window.redraw.surface_probe_at = Some(now);
        match mode {
            0 => assert!(window.redraw.stopped_generation.is_none()),
            1 => window.hidden = true,
            2 => window.redraw.native_occluded = true,
            3 => window.redraw.parked = true,
            4 => window.redraw.stopped_generation = Some(17),
            _ => unreachable!(),
        }
        let pending = window.redraw.snapshot();
        window.finish_surface_probe(
            Err(anyhow::anyhow!("surface creation failed")),
            mode != 0,
            now,
        );
        assert_eq!(window.redraw.surface_probe_at, None);
        assert!(window.redraw.backend_occluded);
        assert!(!window.redraw.request_in_flight);
        assert_eq!(window.redraw.snapshot().0, pending.0);
    }
}

/// The native adapter forwards the actual result and post-probe device reading to the tested retry seam.
#[cfg(target_os = "macos")]
#[test]
fn surface_probe_error_retry_uses_the_actual_post_probe_device_gate() {
    let source = include_str!("redraw.rs");
    let service = &source[source.find("fn service_surface_probe(").unwrap()
        ..source.find("pub(super) fn mark_window_redraw(").unwrap()];
    assert!(
        service.find("renderer.probe_surface_availability()").unwrap()
            < service.find("renderer.device_accepts_gpu_work()").unwrap()
    );
    assert!(service.contains("window.finish_surface_probe(outcome, device_usable, Instant::now())"));
}

/// Source order binds pure owner tests to actual native dispatch, both collector roles, retained-key invalidation, and the B9 stop boundary.
#[test]
fn production_occlusion_order_retained_invalidation_and_device_precedence_are_pinned() {
    let events = include_str!("window_event.rs");
    let start = events.find("pub(super) fn do_window_event(").unwrap();
    let route = &events[start..];
    let warm = route.find("self.is_warm_window_id(win_id)").unwrap();
    let stale = route.find("!self.windows.contains_key(&win_id)").unwrap();
    let occluded = route.find("self.handle_window_occlusion(win_id, *occluded)").unwrap();
    let child = route.find("self.handle_child_window_event(").unwrap();
    assert!(warm < stale && stale < occluded && occluded < child);
    for (source, collect) in [
        (include_str!("window_event.rs"), "self.main_visible_frame_sources("),
        (
            concat!(include_str!("child_window.rs"), include_str!("child_window_redraw.rs")),
            "self.child_visible_frame_sources(",
        ),
    ] {
        assert!(
            source.find("self.begin_window_redraw(win_id,").unwrap()
                < source.find(collect).unwrap()
        );
    }
    let redraw = include_str!("redraw.rs");
    let start = redraw.find("pub(super) fn begin_window_redraw(").unwrap();
    let entry = &redraw
        [start..redraw[start..].find("pub(super) fn snapshot_window_redraw(").unwrap() + start];
    assert!(
        entry.find("smoke.note_stopped_redraw_refusal(").unwrap()
            < entry.find("renderer.take_stopped_render_outcome()").unwrap()
    );
    assert!(
        entry.find("renderer.take_stopped_render_outcome()").unwrap()
            < entry
                .find("window.redraw.native_occluded || window.redraw.backend_occluded")
                .unwrap()
    );
    assert!(
        entry.find("!window.frame_deadlines_allowed()").unwrap()
            < entry.find("window.contention_blocks_redraw(").unwrap()
    );
    assert!(!entry.contains("note_render_attempt"));
    let invalidate = &redraw[redraw.find("pub(super) fn invalidate_visibility_frame(").unwrap()
        ..redraw.find("pub(super) fn request_visible_frame(").unwrap()];
    assert!(invalidate.contains("renderer.invalidate_retained_frame()"));
    assert!(!invalidate.contains(".lock()"));
    let request = &redraw[redraw.find("pub(super) fn request_visible_frame(").unwrap()
        ..redraw.find("pub(super) fn surface_probe_deadline(").unwrap()];
    assert!(
        request.contains("!self.redraw.request_in_flight")
            && request.contains("renderer.device_accepts_gpu_work()")
    );
    assert_eq!(request.matches("self.request_window_redraw()").count(), 1);
}

/// Replacing a stopped backend drops only that device's occlusion, never the native window's visibility state.
#[test]
fn validated_device_recovery_clears_only_backend_occlusion() {
    for native in [false, true] {
        let (mut app, main, _) = owners();
        let stopped = sonicterm_gpu::device_errors::DeviceErrorState::new().snapshot();
        let replacement = sonicterm_gpu::device_errors::DeviceErrorState::new().snapshot();
        let window = app.windows.get_mut(&main).unwrap();
        window.redraw.stopped_generation = Some(stopped.generation);
        window.redraw.backend_occluded = true;
        window.redraw.native_occluded = native;
        assert!(window.request_device_recovery(&replacement));
        assert!(!window.redraw.backend_occluded);
        assert_eq!(window.redraw.native_occluded, native);
        assert_eq!(window.redraw.request_in_flight, !native);
    }
}

/// Arm a visible, settled Auto scrollbar on `owner`'s active pane, active at
/// `active` and last ticked then; returns the pane id. No renderer is attached
/// and the app is not degraded, so the window's motion is Animated (Fade).
fn arm_settled_fade_scrollbar(app: &mut App, owner: WindowId, active: Instant) -> u64 {
    use crate::app::scrollbar_visibility::{note_activity, retarget, ScrollbarVisState};
    app.config.appearance.scrollbar = sonicterm_cfg::config::ScrollbarMode::Auto;
    let window = app.windows.get_mut(&owner).unwrap();
    let pane = window.tab_states[window.tabs.active_index()].active_pane;
    let mut vis = ScrollbarVisState::new(active);
    note_activity(&mut vis, active);
    retarget(&mut vis, sonicterm_cfg::config::ScrollbarMode::Auto, false, active);
    vis.alpha = 1.0;
    window.scrollbar_vis.insert(pane, vis);
    pane
}

/// The scrollbar deadlines `frame_due_work_at` collects for `owner`.
fn scrollbar_due(app: &App, owner: WindowId, now: Instant) -> Vec<Instant> {
    app.frame_due_work_at(now)
        .iter()
        .filter(|work| work.owner == Some(owner) && work.cause == DueCause::Scrollbar)
        .map(|work| work.deadline)
        .collect()
}

#[test]
fn an_animated_idle_deadline_is_serviced_once_and_spares_a_later_owner() {
    // A Fade window's settled bar contributes one owner-local deadline. At it,
    // the bar retargets to hidden and the owner gets one frame request; the
    // deadline is gone from the next collection before that frame arrives,
    // and a second window whose deadline is later is not touched.
    let (mut app, main, child) = owners();
    let active = Instant::now();
    let idle = Duration::from_millis(crate::app::scrollbar_visibility::IDLE_HIDE_MS);
    let main_pane = arm_settled_fade_scrollbar(&mut app, main, active);
    let child_pane =
        arm_settled_fade_scrollbar(&mut app, child, active + Duration::from_millis(100));
    let deadline = active + idle;
    assert_eq!(scrollbar_due(&app, main, active), vec![deadline]);
    let child_before = app.windows[&child].scrollbar_vis.clone();
    app.redraw_due = app.frame_due_work_at(active);
    app.service_redraw_due(deadline - Duration::from_nanos(1));
    assert!(!app.windows[&main].redraw.request_in_flight);
    app.service_redraw_due(deadline);
    let vis = app.windows[&main].scrollbar_vis[&main_pane];
    assert!(app.windows[&main].redraw.request_in_flight, "one frame starts the fade");
    assert_eq!((vis.alpha, vis.target, vis.idle_consumed), (1.0, 0.0, true));
    assert!(scrollbar_due(&app, main, deadline).is_empty(), "the deadline cannot re-fire");
    assert_eq!(app.windows[&child].scrollbar_vis, child_before);
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert_eq!(
        scrollbar_due(&app, child, deadline),
        vec![active + Duration::from_millis(100) + idle]
    );
    assert!(!app.windows[&child].scrollbar_vis[&child_pane].idle_consumed);
}

#[test]
fn hidden_occluded_and_parked_owners_contribute_no_idle_deadline() {
    // The idle deadline is a frame-family deadline: an owner that cannot
    // present must not wake the loop for its scrollbar.
    for state in ["hidden", "occluded", "parked"] {
        let (mut app, main, _) = owners();
        let active = Instant::now();
        arm_settled_fade_scrollbar(&mut app, main, active);
        assert_eq!(scrollbar_due(&app, main, active).len(), 1, "{state}: visible baseline");
        match state {
            "hidden" => app.windows.get_mut(&main).unwrap().hidden = true,
            "occluded" => app.windows.get_mut(&main).unwrap().redraw.native_occluded = true,
            _ => {
                park_owner(&mut app, main);
            }
        }
        assert!(scrollbar_due(&app, main, active).is_empty(), "{state}");
    }
}

#[test]
fn activity_or_a_hold_after_collection_makes_the_expiry_a_no_op() {
    // Expiry work collected before fresh activity, an edge hover or a drag
    // must not hide the bar or request a frame when it comes due.
    for case in ["activity", "hover", "drag"] {
        let (mut app, main, _) = owners();
        let active = Instant::now();
        let idle = Duration::from_millis(crate::app::scrollbar_visibility::IDLE_HIDE_MS);
        let pane = arm_settled_fade_scrollbar(&mut app, main, active);
        let deadline = active + idle;
        app.redraw_due = app.frame_due_work_at(active);
        let fresh = deadline - Duration::from_millis(50);
        let mode = sonicterm_cfg::config::ScrollbarMode::Auto;
        let window = app.windows.get_mut(&main).unwrap();
        match case {
            "activity" => window.note_scrollbar_activity(pane, mode, fresh),
            "hover" => {
                let rects = [(pane, 0.0, 0.0, 800.0, 480.0)];
                assert!(crate::app::scrollbar_visibility::update_hover_states(
                    &mut window.scrollbar_vis,
                    &rects,
                    (795.0, 100.0),
                    mode,
                    None,
                    fresh,
                ));
            }
            _ => {
                window.begin_scrollbar_drag(
                    crate::app::scrollbar_input::ScrollbarDragState {
                        pane_id: pane,
                        geometry: sonicterm_ui::scrollbar::ScrollbarGeometry {
                            track_rect: sonicterm_ui::scrollbar::Rect {
                                x: 792.0,
                                y: 0.0,
                                w: 8.0,
                                h: 480.0,
                            },
                            thumb_rect: sonicterm_ui::scrollbar::Rect {
                                x: 792.0,
                                y: 0.0,
                                w: 8.0,
                                h: 48.0,
                            },
                        },
                        press_y: 10.0,
                        grab_offset: 10.0,
                        viewport_rows: 24,
                        total_rows: 240,
                    },
                    mode,
                    fresh,
                );
            }
        }
        let before = app.windows[&main].scrollbar_vis[&pane];
        app.service_redraw_due(deadline);
        let window = &app.windows[&main];
        assert_eq!(window.scrollbar_vis[&pane], before, "{case}: no target change");
        assert_eq!(window.scrollbar_vis[&pane].target, 1.0, "{case}");
        assert!(!window.redraw.request_in_flight, "{case}: no frame request");
    }
}

#[test]
fn an_overdue_expiry_after_an_earlier_retarget_requests_the_fade_frame() {
    // A release retargeted the bar to hidden but no frame ran. When the overdue
    // idle deadline is serviced, the target is already 0, yet alpha is still 1:
    // the owner must get a frame, or the bar stays shown.
    let (mut app, main, _) = owners();
    let now = Instant::now();
    let active = now.checked_sub(Duration::from_secs(2)).unwrap();
    let pane = arm_settled_fade_scrollbar(&mut app, main, active);
    let mode = sonicterm_cfg::config::ScrollbarMode::Auto;
    let window = app.windows.get_mut(&main).unwrap();
    let vis = window.scrollbar_vis.get_mut(&pane).unwrap();
    assert!(crate::app::scrollbar_visibility::retarget(vis, mode, false, now));
    window.redraw.request_in_flight = false;
    app.redraw_due = app.frame_due_work_at(now);
    assert!(app
        .redraw_due
        .iter()
        .any(|work| work.owner == Some(main) && work.cause == DueCause::Scrollbar));
    app.service_redraw_due(now);
    let window = &app.windows[&main];
    assert!(window.scrollbar_vis[&pane].idle_consumed);
    assert_eq!(window.scrollbar_vis[&pane].alpha, 1.0);
    assert!(window.redraw.request_in_flight, "the still-fading owner gets its frame");
}

/// A headless main window whose redraw state is settled and whose image atlas release is injected:
/// promoted, with its 30 s deadline at `deadline`.
fn idle_release_owner(deadline: Instant) -> (App, WindowId) {
    let (mut app, main, _child) = owners();
    let window = app.windows.get_mut(&main).unwrap();
    let snapshot = window.redraw.snapshot();
    window.redraw.settle(snapshot, FrameSettlement::Settled, deadline);
    window.redraw.deferred = false;
    window.redraw.request_in_flight = false;
    window.test_image_atlas_release =
        Some(TestImageAtlasRelease { deadline: Some(deadline), releases: 0 });
    (app, main)
}

fn release_work(app: &App, now: Instant) -> Vec<WindowId> {
    app.frame_due_work_at(now)
        .into_iter()
        .filter(|work| work.cause == DueCause::ImageAtlasRelease)
        .filter_map(|work| work.owner)
        .collect()
}

fn releases(app: &App, id: WindowId) -> u32 {
    app.windows[&id].test_image_atlas_release.as_ref().unwrap().releases
}

/// A release is collected only for an idle, visible window at or after its deadline; every frame-family
/// blocker (deferred, a request in flight, a pending cause, hidden, occluded, parked) excludes it.
#[test]
fn image_atlas_release_requires_an_idle_visible_window() {
    let now = Instant::now();
    let (app, main) = idle_release_owner(now);
    assert!(app.windows[&main].image_atlas_release_eligible(now));
    assert!(!app.windows[&main].image_atlas_release_eligible(now - Duration::from_millis(1)));
    assert_eq!(release_work(&app, now), vec![main], "the deadline is collected for the wake");
    let blockers: [(&str, fn(&mut WindowState)); 6] = [
        ("deferred", |window| window.redraw.deferred = true),
        ("request_in_flight", |window| window.redraw.request_in_flight = true),
        ("has_pending", |window| window.mark_redraw(RedrawCause::Output)),
        ("hidden", |window| window.hidden = true),
        ("occluded", |window| window.redraw.native_occluded = true),
        ("parked", |window| window.redraw.parked = true),
    ];
    for (name, block) in blockers {
        let (mut app, main) = idle_release_owner(now);
        block(app.windows.get_mut(&main).unwrap());
        assert!(!app.windows[&main].image_atlas_release_eligible(now), "{name} must block");
        assert!(release_work(&app, now).is_empty(), "{name} must not be collected");
        app.redraw_due =
            vec![DueWork { owner: Some(main), cause: DueCause::ImageAtlasRelease, deadline: now }];
        app.service_redraw_due(now);
        assert_eq!(releases(&app, main), 0, "{name} must block servicing");
    }
}

/// An eligible release runs once, asks for no frame and leaves no deadline behind.
#[test]
fn an_eligible_release_runs_once_without_a_frame() {
    let now = Instant::now();
    let (mut app, main) = idle_release_owner(now);
    let requests = super::super::window_state::window_redraw_requests();
    app.redraw_due = app.frame_due_work_at(now);
    app.service_redraw_due(now);
    assert_eq!(releases(&app, main), 1);
    assert_eq!(super::super::window_state::window_redraw_requests(), requests, "no native request");
    assert!(!app.windows[&main].redraw.request_in_flight);
    assert!(!app.windows[&main].redraw.has_pending(), "no repaint cause was marked");
    assert!(release_work(&app, now).is_empty(), "the released atlas arms no deadline");
    app.service_redraw_due(now);
    assert_eq!(releases(&app, main), 1);
}

/// Work marked after collection blocks the release at service time; the item is dropped, not re-queued,
/// and the next collection after the frame settles picks it up again.
#[test]
fn work_pending_between_collection_and_service_defers_the_release() {
    let now = Instant::now();
    let (mut app, main) = idle_release_owner(now);
    app.redraw_due = app.frame_due_work_at(now);
    app.windows.get_mut(&main).unwrap().mark_redraw(RedrawCause::Output);
    app.service_redraw_due(now);
    assert_eq!(releases(&app, main), 0);
    assert!(
        !app.redraw_due.iter().any(|work| work.cause == DueCause::ImageAtlasRelease),
        "a blocked release is dropped, so it cannot spin"
    );
    let window = app.windows.get_mut(&main).unwrap();
    let snapshot = window.redraw.snapshot();
    window.redraw.settle(snapshot, FrameSettlement::Presented, now);
    assert_eq!(release_work(&app, now), vec![main], "collected again once eligible");
}

/// A due frame and a due release together: the frame implies `deferred`, so the release is skipped and
/// the frame is requested, in either order.
#[test]
fn a_coinciding_frame_deadline_skips_the_release() {
    let now = Instant::now();
    for frame_first in [true, false] {
        let (mut app, main) = idle_release_owner(now);
        app.windows.get_mut(&main).unwrap().redraw.deferred = true;
        let frame = DueWork { owner: Some(main), cause: DueCause::Frame, deadline: now };
        let release =
            DueWork { owner: Some(main), cause: DueCause::ImageAtlasRelease, deadline: now };
        app.redraw_due = if frame_first { vec![frame, release] } else { vec![release, frame] };
        app.service_redraw_due(now);
        assert_eq!(releases(&app, main), 0, "frame_first={frame_first}");
        assert!(app.windows[&main].redraw.request_in_flight, "the frame is requested");
    }
}

/// A firing whose deadline is gone (media returned, or the atlas was already released) or later than
/// now does nothing.
#[test]
fn a_stale_release_firing_does_nothing() {
    let now = Instant::now();
    for deadline in [None, Some(now + Duration::from_secs(1))] {
        let (mut app, main) = idle_release_owner(now);
        app.windows.get_mut(&main).unwrap().test_image_atlas_release.as_mut().unwrap().deadline =
            deadline;
        app.redraw_due =
            vec![DueWork { owner: Some(main), cause: DueCause::ImageAtlasRelease, deadline: now }];
        app.service_redraw_due(now);
        assert_eq!(releases(&app, main), 0);
    }
}

/// The pane's decoded media is a separate allocation from the renderer's atlas: a serviced release leaves
/// the pane's `InlineMediaRetained` charge exactly as it was, both before and after the next charge.
#[test]
fn an_image_atlas_release_leaves_the_pane_media_charge_unchanged() {
    let now = Instant::now();
    let (mut app, main) = idle_release_owner(now);
    let pane_id = *app.windows[&main].panes.keys().next().unwrap();
    let image = sonicterm_render_model::InlineImage {
        id: 1,
        row: 0,
        col: 0,
        width: 32,
        height: 32,
        bgra: Arc::from(vec![0; 32 * 32 * 4]),
    };
    assert!(app.__test_set_pane_inline_images(main, pane_id, vec![image]));
    app.reconcile_pane_owners();
    app.__test_charge_pane_owners();
    let media = |app: &App| {
        app.__test_pane_charges(main, pane_id).unwrap()
            [&sonicterm_types::ResourceClass::InlineMediaRetained]
    };
    let charged = media(&app);
    assert!(charged.bytes >= 32 * 32 * 4, "precondition: the decoded image is charged");
    assert_eq!(app.__test_collect_and_service_redraw_due(now), 1, "the release is collected");
    assert_eq!(releases(&app, main), 1, "and serviced");
    assert_eq!(media(&app), charged, "the release itself moves no pane charge");
    app.__test_charge_pane_owners();
    assert_eq!(media(&app), charged, "nor does the next charge");
}

/// The gated sampling seam keeps production's interval: a pass inside it does nothing, and the first pass
/// at or past the interval runs.
#[test]
fn the_gated_retention_seam_keeps_the_sampling_interval() {
    let (mut app, _main) = idle_release_owner(Instant::now());
    let start = Instant::now();
    assert!(app.__test_sample_pane_retention_at(start), "the first pass always runs");
    let interval = super::super::retention::RETENTION_SAMPLE_INTERVAL;
    assert!(!app.__test_sample_pane_retention_at(start + interval - Duration::from_millis(1)));
    assert!(app.__test_sample_pane_retention_at(start + interval));
}

/// Output in a background tab of a visible window requests no frame and marks no cause, and the
/// token is acknowledged; a command badge that changes in that tab still requests exactly one.
#[test]
fn background_tab_output_requests_no_frame_but_a_badge_change_requests_one() {
    use crate::app::output_event::OutputEvent;
    use std::sync::atomic::Ordering;
    let (mut app, _, child) = owners();
    let pane = app.windows[&child].tab_states[1].active_pane;
    let state = &app.windows[&child].panes[&pane];
    state.output_generation.fetch_add(1, Ordering::Release);
    state.output_outstanding.store(true, Ordering::Release);
    let causes = app.windows[&child].redraw.snapshot();
    let before = crate::app::window_state::window_redraw_requests();

    app.service_output_event(OutputEvent::Pane { window_id: child, pane_id: pane }, Instant::now());

    assert_eq!(crate::app::window_state::window_redraw_requests(), before, "no native request");
    assert_eq!(app.windows[&child].redraw.snapshot(), causes, "no cause marked");
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert!(!app.windows[&child].panes[&pane].output_outstanding.load(Ordering::Acquire));

    let now = Instant::now();
    let state = &app.windows[&child].panes[&pane];
    state.command_events.lock().push(crate::app::PaneCommandEvent {
        event: sonicterm_vt::vt::CommandEvent::CmdEnd(Some(0)),
        at: now,
        duration: None,
    });
    state.output_outstanding.store(true, Ordering::Release);
    let output = app.windows[&child].redraw.cause_generation(RedrawCause::Output);

    app.service_output_event(OutputEvent::Pane { window_id: child, pane_id: pane }, now);

    assert_eq!(crate::app::window_state::window_redraw_requests(), before + 1, "one request");
    assert!(app.windows[&child].redraw.request_in_flight);
    assert!(
        app.windows[&child].redraw.cause_generation(RedrawCause::Chrome)
            > causes.0[RedrawCause::Chrome as usize]
    );
    assert_eq!(app.windows[&child].redraw.cause_generation(RedrawCause::Output), output);
}

/// An explicit request for a window whose output is settled (as the harness sends after clearing
/// its retained frame) still makes exactly one native request with an `Output` cause.
#[test]
fn explicit_request_with_settled_output_requests_one_output_frame() {
    use crate::app::output_event::OutputEvent;
    let (mut app, _, child) = owners();
    let pane = app.windows[&child].tab_states[0].active_pane;
    let state = &app.windows[&child].panes[&pane];
    assert_eq!(
        state.output_generation.load(std::sync::atomic::Ordering::Acquire),
        state.observed_output_generation
    );
    if let Some(renderer) = app.windows.get_mut(&child).unwrap().renderer.as_mut() {
        renderer.invalidate_retained_frame();
    }
    let output = app.windows[&child].redraw.cause_generation(RedrawCause::Output);
    let before = crate::app::window_state::window_redraw_requests();

    app.service_output_event(OutputEvent::Explicit(child), Instant::now());

    assert_eq!(crate::app::window_state::window_redraw_requests(), before + 1);
    assert!(app.windows[&child].redraw.cause_generation(RedrawCause::Output) > output);
}

/// Owners whose App counts frames, so a test can read each window's deferral rule counts.
fn counting_owners() -> (App, WindowId, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child", "background"]);
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    (app, main, child)
}

/// Move both of a window's pacing clocks back, so pacing does not depend on when the test built it.
fn backdate_clocks(app: &mut App, id: WindowId, at: Instant) {
    let window = app.windows.get_mut(&id).unwrap();
    window.last_render = at;
    window.stream_clock = at;
}

/// Publish one completed output batch on the window's visible active pane.
fn publish_visible_output(app: &App, id: WindowId) {
    let window = &app.windows[&id];
    let pane = window.tab_states[window.tabs.active_index()].active_pane;
    window.panes[&pane].output_generation.fetch_add(1, Ordering::Release);
}

/// Run one admitted attempt for `cause` through the production begin, snapshot and finish adapters.
fn admitted_attempt(
    app: &mut App,
    id: WindowId,
    cause: RedrawCause,
    outcome: FrameSettlement,
    at: Instant,
) {
    app.mark_window_redraw(id, cause);
    assert!(app.begin_window_redraw(id, at), "{cause:?} attempt at the test instant is admitted");
    let snapshot = app.snapshot_window_redraw_at(id, at).unwrap();
    app.finish_window_redraw(id, &snapshot, outcome, at);
}

/// How many times this window deferred under `rule`.
fn defer_count(app: &App, id: WindowId, rule: super::super::frame_counters::DeferRule) -> u64 {
    use super::super::frame_counters::DeferRule;
    let counters = app.windows[&id].redraw.frame_counters.as_deref().expect("counting App");
    match rule {
        DeferRule::Timeout => counters.defer_timeout,
        DeferRule::Contention => counters.defer_contention,
        DeferRule::Streaming => counters.defer_streaming,
    }
}

/// A hardware keypress frame that presented nothing must not delay the shell's echo by one period.
#[test]
fn echo_after_a_settled_keypress_is_not_paced_from_the_keypress() {
    use super::super::frame_counters::DeferRule;
    for child_owner in [false, true] {
        let (mut app, main, child) = counting_owners();
        let owner = if child_owner { child } else { main };
        let keypress_at = Instant::now() + Duration::from_secs(1);
        backdate_clocks(&mut app, owner, keypress_at - Duration::from_secs(1));
        // An output-only frame presented 20 ms before the keypress.
        publish_visible_output(&app, owner);
        admitted_attempt(
            &mut app,
            owner,
            RedrawCause::Output,
            FrameSettlement::Presented,
            keypress_at - Duration::from_millis(20),
        );
        // The keypress frame finds no echo yet and settles without presenting.
        admitted_attempt(
            &mut app,
            owner,
            RedrawCause::Input,
            FrameSettlement::Settled,
            keypress_at,
        );
        // The echo arrives 4 ms later: 24 ms after the last streaming frame, more than a 60 Hz period.
        publish_visible_output(&app, owner);
        app.mark_window_redraw(owner, RedrawCause::Output);
        assert!(
            app.begin_window_redraw(owner, keypress_at + Duration::from_millis(4)),
            "the echo is admitted at once (child: {child_owner})"
        );
        assert_eq!(defer_count(&app, owner, DeferRule::Streaming), 0);
        let counters = app.windows[&owner].redraw.frame_counters.as_deref().unwrap();
        assert_eq!(counters.stream_clock_exempt, 1, "only the keypress kept the streaming clock");
    }
}

/// For every deferral rule, the armed Frame deadline is the first instant begin admits.
#[test]
fn frame_deadlines_match_admission_for_every_rule() {
    use super::super::frame_counters::DeferRule;
    let period = Duration::from_micros(16_667);
    for case in 0..4 {
        let (mut app, main, _) = counting_owners();
        let start = Instant::now() + Duration::from_secs(1);
        backdate_clocks(&mut app, main, start - Duration::from_secs(1));
        let (rule, deadline, probe) = match case {
            0 => {
                // Streaming after a presented input frame.
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Input,
                    FrameSettlement::Presented,
                    start,
                );
                (DeferRule::Streaming, start + period, start + Duration::from_millis(1))
            }
            1 => {
                // Streaming after an exempt settled keypress that followed a non-input attempt at `start`.
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Output,
                    FrameSettlement::Presented,
                    start,
                );
                let keypress = start + Duration::from_millis(5);
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Input,
                    FrameSettlement::Settled,
                    keypress,
                );
                (DeferRule::Streaming, start + period, keypress + Duration::from_millis(1))
            }
            2 => {
                // A surface timeout waits one period from the attempt.
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Input,
                    FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
                    start,
                );
                (DeferRule::Timeout, start + period, start + Duration::from_millis(1))
            }
            _ => {
                // A contention floor later than the paced instant wins.
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Input,
                    FrameSettlement::Presented,
                    start,
                );
                app.windows.get_mut(&main).unwrap().retry_not_before = Some(start + period * 2);
                (DeferRule::Contention, start + period * 2, start + Duration::from_millis(1))
            }
        };
        publish_visible_output(&app, main);
        app.mark_window_redraw(main, RedrawCause::Output);
        assert!(!app.begin_window_redraw(main, probe), "case {case} defers");
        let frames: Vec<_> = app
            .frame_due_work_at(probe)
            .into_iter()
            .filter(|work| work.owner == Some(main) && work.cause == DueCause::Frame)
            .collect();
        assert_eq!(frames.len(), 1, "case {case} arms one frame deadline");
        assert_eq!(frames[0].deadline, deadline, "case {case} deadline");
        let before = defer_count(&app, main, rule);
        assert!(!app.begin_window_redraw(main, deadline - Duration::from_nanos(1)), "case {case}");
        assert_eq!(defer_count(&app, main, rule), before + 1, "case {case} defers with {rule:?}");
        assert!(app.begin_window_redraw(main, deadline), "case {case} admits at its deadline");
    }
}

/// The text of the method `signature` names, up to the closing brace of an impl-level method.
fn method_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let end = source[start..].find("\n    }\n").map_or(source.len(), |offset| start + offset);
    &source[start..end]
}

/// The child adapter completes through `finish_window_redraw`, the seam the behavioural tests drive
/// for both roles, passing the renderer's own settlement and instant; it writes no clock, settles no
/// cause and picks no software policy itself, so a wrong argument cannot hide in a second writer.
#[test]
fn main_and_child_completions_share_one_clock_writer() {
    let child_source = include_str!("child_window_redraw.rs").replace("\r\n", "\n");
    let main_source = include_str!("window_event.rs").replace("\r\n", "\n");
    let redraw_source = include_str!("redraw.rs").replace("\r\n", "\n");
    let child_call = "self.finish_window_redraw(win_id, snapshot, settlement, at);";
    assert_eq!(child_source.matches("finish_window_redraw(").count(), 1, "one child completion");
    assert!(
        child_source.contains(child_call),
        "the child passes its own snapshot, outcome and instant"
    );
    assert!(
        child_source.contains("let settlement = super::redraw::FrameSettlement::of(&outcome);"),
        "the child's outcome is the renderer's"
    );
    assert!(
        child_source.contains("frame_completion = Some((settlement, Instant::now()));"),
        "the instant is taken after the renderer call"
    );
    for inline in [
        ".complete_attempt(",
        "last_render =",
        "stream_clock",
        ".settle(",
        "software_render_degrade",
    ] {
        assert!(!child_source.contains(inline), "the child adapter must not use {inline} itself");
    }
    assert_eq!(
        main_source.matches("self.finish_window_redraw(win_id, snapshot, outcome, at);").count(),
        1
    );
    assert!(
        method_body(&redraw_source, "pub(super) fn finish_window_redraw(").contains(
            "window.complete_attempt(snapshot, outcome, at, self.software_render_degrade);"
        ),
        "completion reads the App's software policy, never a caller's"
    );
}

/// A surface timeout waits one period from its own attempt, even when an exempt keypress came first.
#[test]
fn surface_timeout_retry_still_waits_one_period_from_the_attempt() {
    use super::super::frame_counters::DeferRule;
    let period = Duration::from_micros(16_667);
    for after_keypress in [false, true] {
        let (mut app, main, _) = counting_owners();
        let start = Instant::now() + Duration::from_secs(1);
        backdate_clocks(&mut app, main, start - Duration::from_secs(1));
        let timeout_at = if after_keypress {
            admitted_attempt(&mut app, main, RedrawCause::Input, FrameSettlement::Settled, start);
            start + Duration::from_millis(1)
        } else {
            start
        };
        admitted_attempt(
            &mut app,
            main,
            RedrawCause::Input,
            FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
            timeout_at,
        );
        let deadline = timeout_at + period;
        let frames: Vec<_> = app
            .frame_due_work_at(timeout_at)
            .into_iter()
            .filter(|work| work.owner == Some(main) && work.cause == DueCause::Frame)
            .collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].deadline, deadline, "the deadline comes from the attempt clock");
        assert!(!app.begin_window_redraw(main, deadline - Duration::from_nanos(1)));
        assert_eq!(defer_count(&app, main, DeferRule::Timeout), 1);
        assert!(app.begin_window_redraw(main, deadline), "the retry is admitted at its deadline");
    }
}

/// Output-only attempts stay one per period, even with exempt keypresses interleaved at 240 Hz.
#[test]
fn unchanged_non_input_passes_stay_one_per_period() {
    let period = Duration::from_micros(16_667);
    let horizon_ms = 200_u32;
    let bound = u64::from(horizon_ms * 1_000).div_ceil(16_667) + 1;
    for with_input in [false, true] {
        let (mut app, main, _) = counting_owners();
        let start = Instant::now() + Duration::from_secs(1);
        backdate_clocks(&mut app, main, start - period * 2);
        let mut output_admitted = 0_u64;
        let mut input_events = 0_u64;
        let mut last_output: Option<Instant> = None;
        for tick_ms in 0..horizon_ms {
            let at = start + Duration::from_millis(u64::from(tick_ms));
            publish_visible_output(&app, main);
            app.mark_window_redraw(main, RedrawCause::Output);
            // Every 4 ms is about 240 Hz of keypresses.
            if with_input && tick_ms % 4 == 0 {
                app.mark_window_redraw(main, RedrawCause::Input);
                input_events += 1;
            }
            let carries_input = app.windows[&main].redraw.input_pending();
            if !app.begin_window_redraw(main, at) {
                continue;
            }
            let snapshot = app.snapshot_window_redraw_at(main, at).unwrap();
            app.finish_window_redraw(main, &snapshot, FrameSettlement::Settled, at);
            if !carries_input {
                // When: the admitted attempt carried no input, it must be a period after the previous one.
                if let Some(previous) = last_output {
                    assert!(at - previous >= period, "output passes {previous:?} and {at:?}");
                }
                last_output = Some(at);
                output_admitted += 1;
            }
        }
        let exempt =
            app.windows[&main].redraw.frame_counters.as_deref().unwrap().stream_clock_exempt;
        assert!(output_admitted <= bound, "{output_admitted} > {bound}");
        assert!(exempt <= input_events, "exempt attempts never outnumber input events");
        assert_eq!(exempt > 0, with_input, "the exemption is exercised only with input");
    }
}

/// Under both software policies and for both window roles, every outcome moves both clocks and
/// counts no exemption, except the one exempt case: a settled hardware keypress with no retry floor.
#[test]
fn outcomes_that_advance_the_streaming_clock() {
    let mut outcomes = vec![
        FrameSettlement::Presented,
        FrameSettlement::Cached,
        FrameSettlement::Settled,
        FrameSettlement::Retry(RedrawCause::AtlasRetry),
        FrameSettlement::Retry(RedrawCause::SurfaceRetry),
        FrameSettlement::Failed,
        FrameSettlement::Stopped(3),
    ];
    for reason in [
        SurfaceRetryReason::Timeout,
        SurfaceRetryReason::Occluded,
        SurfaceRetryReason::Outdated,
        SurfaceRetryReason::Suboptimal,
        SurfaceRetryReason::SurfaceLost,
    ] {
        outcomes.push(FrameSettlement::SurfaceRetry(reason));
    }
    let mut cases = Vec::new();
    for software in [false, true] {
        for outcome in &outcomes {
            // The settled hardware keypress is the exempt case; its own tests cover it.
            if *outcome != FrameSettlement::Settled || software {
                cases.push((RedrawCause::Input, *outcome, false, software));
            }
        }
        // A settled attempt without a new input generation, and a settled keypress under a contention floor.
        cases.push((RedrawCause::Expose, FrameSettlement::Settled, false, software));
        cases.push((RedrawCause::Input, FrameSettlement::Settled, true, software));
    }
    for child_owner in [false, true] {
        for (cause, outcome, retry_armed, software) in cases.iter().copied() {
            let (mut app, main, child) = counting_owners();
            let owner = if child_owner { child } else { main };
            app.software_render_degrade = software;
            let now = Instant::now() + Duration::from_secs(1);
            backdate_clocks(&mut app, owner, now - Duration::from_secs(1));
            if retry_armed {
                app.windows.get_mut(&owner).unwrap().retry_not_before =
                    Some(now + Duration::from_secs(1));
            }
            app.mark_window_redraw(owner, cause);
            let snapshot = app.snapshot_window_redraw_at(owner, now).unwrap();
            app.finish_window_redraw(owner, &snapshot, outcome, now);
            let window = &app.windows[&owner];
            let case = format!(
                "child={child_owner} {cause:?} {outcome:?} retry={retry_armed} software={software}"
            );
            assert_eq!(window.last_render, now, "{case}");
            assert_eq!(window.stream_clock, now, "{case}");
            assert_eq!(
                window.redraw.frame_counters.as_deref().unwrap().stream_clock_exempt,
                0,
                "{case}"
            );
        }
    }
}

/// Degraded software keeps its exact 25 ms cadence after a keypress, and 83,333 µs while composing.
#[test]
fn degraded_software_keeps_its_cadence_after_a_keypress() {
    use super::super::frame_counters::DeferRule;
    for composing in [false, true] {
        for outcome in [FrameSettlement::Settled, FrameSettlement::Cached] {
            let (mut app, main, _) = counting_owners();
            app.software_render_degrade = true;
            let period = if composing {
                Duration::from_micros(83_333)
            } else {
                Duration::from_micros(25_000)
            };
            if composing {
                app.windows.get_mut(&main).unwrap().ime.handle_preedit("中", None);
            }
            let at = Instant::now() + Duration::from_secs(1);
            backdate_clocks(&mut app, main, at - Duration::from_secs(1));
            app.mark_window_redraw(main, RedrawCause::Input);
            let snapshot = app.snapshot_window_redraw_at(main, at).unwrap();
            app.finish_window_redraw(main, &snapshot, outcome, at);
            publish_visible_output(&app, main);
            app.mark_window_redraw(main, RedrawCause::Output);
            assert!(!app.begin_window_redraw(main, at + Duration::from_micros(1)));
            assert_eq!(defer_count(&app, main, DeferRule::Streaming), 1);
            let frames: Vec<_> = app
                .frame_due_work_at(at)
                .into_iter()
                .filter(|work| work.owner == Some(main) && work.cause == DueCause::Frame)
                .collect();
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].deadline, at + period, "composing={composing} {outcome:?}");
            assert!(!app.begin_window_redraw(main, at + period - Duration::from_micros(1)));
            assert!(app.begin_window_redraw(main, at + period), "admitted at exactly one period");
        }
    }
}
