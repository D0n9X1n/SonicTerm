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
        (include_str!("child_window.rs"), "poll_command_events_for_child_window(child, &config);"),
    ] {
        let poll = source.find(command_poll).unwrap();
        let capture = source
            .find("sources.try_collect(|| self.snapshot_window_redraw(win_id))")
            .expect("each production role uses the scheduler snapshot before its parser locks");
        let render = source.find("let outcome = r.render_with_outcome(").unwrap();
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

/// Settled no-op/cached frames do not turn retained grid dirt into scheduling work; retries spend input once.
#[test]
fn repeated_outcomes_spend_input_immediacy_without_dirty_row_heartbeats() {
    for outcome in [
        FrameSettlement::Settled,
        FrameSettlement::Cached,
        FrameSettlement::Presented,
        FrameSettlement::Retry(RedrawCause::SurfaceRetry),
        FrameSettlement::Failed,
    ] {
        let (mut app, main, _) = owners();
        let pane = app.windows[&main].tab_states[0].active_pane;
        app.windows[&main].panes[&pane].parser.lock().grid_mut().mark_all_dirty();
        let now = Instant::now();
        app.mark_window_redraw(main, RedrawCause::Input);
        let snapshot = app.snapshot_window_redraw(main).unwrap();
        app.finish_window_redraw(main, &snapshot, outcome, now);
        assert_eq!(app.windows[&main].last_render, now);
        assert!(!app.windows[&main].redraw.input_pending());
        assert!(app.windows[&main].panes[&pane].parser.lock().grid().dirty_count() > 0);
        assert!(app.frame_due_work().is_empty(), "dirt or retained causes alone never arm a timer");
        assert!(
            !app.begin_window_redraw(main, now + Duration::from_micros(1)),
            "presenter-owned repeats stay paced"
        );
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
    for source in [include_str!("window_event.rs"), include_str!("child_window.rs")] {
        assert!(source.contains("sources.try_collect(|| self.snapshot_window_redraw(win_id))"));
        let render = source.find("let outcome = r.render_with_outcome(").unwrap();
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
                    vis.mark_active(active);
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
                assert_eq!(vis.alpha, 0.0);
                assert_eq!(vis.last_active, None);
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
            < request.find("self.request_redraw()").unwrap()
    );
    assert!(request.contains("self.frame_deadlines_allowed() && !self.redraw.request_in_flight"));
    assert_eq!(request.matches("self.request_redraw()").count(), 1);
    let service = &source[source.find("pub(super) fn service_redraw_due(").unwrap()..];
    assert!(
        service.contains("window.frame_deadlines_allowed() && !window.redraw.request_in_flight")
    );
    assert_eq!(service.matches("window.request_redraw()").count(), 1);
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
        window.retry_not_before = Some(now + Duration::from_secs(5));
        window.notification = Some(NotificationBubble {
            level: NotificationLevel::Info,
            message: "expires".into(),
            expires_at: Some(now + Duration::from_millis(10)),
        });
        let mut vis = ScrollbarVisState::new(now);
        vis.mark_active(now);
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
        (include_str!("child_window.rs"), "self.child_visible_frame_sources("),
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
    assert_eq!(request.matches("self.request_redraw()").count(), 1);
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
