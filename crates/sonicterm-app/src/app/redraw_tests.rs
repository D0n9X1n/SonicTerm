use super::*;
pub(super) use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
pub(super) use sonicterm_ui::{
    overlays::{NotificationBubble, NotificationLevel},
    pane::Rect,
};
pub(super) use std::sync::Arc;

/// Real owner state without a native renderer, so fake-clock assertions exercise production adapters.
pub(super) fn owners() -> (App, WindowId, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child", "background"]);
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    (app, main, child)
}

/// One attempt for `cause` ending in `outcome` moves both clocks and is followed by a paced repeat.
pub(super) fn assert_paced_repeat(cause: RedrawCause, outcome: FrameSettlement) {
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

/// Give both roles real resize-adapter geometry and enough tabs to preserve each owner after a removal.
pub(super) fn topology_owners() -> (App, WindowId, WindowId) {
    let (mut app, main, child) = owners();
    app.__test_seed_tab("main survivor");
    app.windows.get_mut(&main).unwrap().tabs.activate(0);
    app.__test_set_main_pane_viewport(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
    app.windows.get_mut(&child).unwrap().test_pane_viewport =
        Some((Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0));
    (app, main, child)
}

/// Enter the same parked scheduler state as a rejected collector, without manufacturing invalid pane geometry.
pub(super) fn park_owner(app: &mut App, id: WindowId) -> u64 {
    let window = app.windows.get_mut(&id).unwrap();
    let snapshot = window.redraw.snapshot();
    window.redraw.park(snapshot);
    window.redraw.request_in_flight = false;
    snapshot.0[RedrawCause::Topology as usize]
}

/// Count the exact production collector adapter invocation behind its real owner gate, without a native event loop.
pub(super) fn gated_collector_calls(app: &mut App, owner: WindowId, now: Instant) -> usize {
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

/// A failed native probe keeps only its owner's slow retry alive while preserving dirt and suppressing assembly.
#[cfg(target_os = "macos")]
#[test]
pub(super) fn surface_probe_errors_rearm_without_consuming_frame_or_output_identity() {
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
pub(super) fn surface_probe_errors_do_not_rearm_suppressed_or_unusable_owners() {
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
pub(super) fn surface_probe_error_retry_uses_the_actual_post_probe_device_gate() {
    let source = concat!(
        include_str!("redraw.rs"),
        "\n",
        include_str!("redraw_admission.rs"),
        "\n",
        include_str!("redraw_state.rs"),
        "\n",
        include_str!("redraw_window.rs")
    );
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
pub(super) fn production_occlusion_order_retained_invalidation_and_device_precedence_are_pinned() {
    let events = concat!(
        include_str!("window_event.rs"),
        "\n",
        include_str!("window_event_pointer.rs"),
        "\n",
        include_str!("window_event_redraw.rs")
    );
    let start = events.find("pub(super) fn do_window_event(").unwrap();
    let route = &events[start..];
    let warm = route.find("self.is_warm_window_id(win_id)").unwrap();
    let stale = route.find("!self.windows.contains_key(&win_id)").unwrap();
    let occluded = route.find("self.handle_window_occlusion(win_id, *occluded)").unwrap();
    let child = route.find("self.handle_child_window_event(").unwrap();
    assert!(warm < stale && stale < occluded && occluded < child);
    for (source, collect) in [
        (
            concat!(
                include_str!("window_event.rs"),
                "\n",
                include_str!("window_event_pointer.rs"),
                "\n",
                include_str!("window_event_redraw.rs")
            ),
            "self.main_visible_frame_sources(",
        ),
        (
            concat!(include_str!("child_window.rs"), include_str!("child_window_redraw.rs")),
            "self.child_visible_frame_sources(",
        ),
    ] {
        assert!(
            source.find("self.admit_window_redraw(win_id)").unwrap()
                < source.find(collect).unwrap()
        );
    }
    let redraw = concat!(
        include_str!("redraw.rs"),
        "\n",
        include_str!("redraw_admission.rs"),
        "\n",
        include_str!("redraw_state.rs"),
        "\n",
        include_str!("redraw_window.rs")
    );
    let start = redraw.find("fn begin_window_redraw(").unwrap();
    let entry = &redraw[start..redraw[start..].find("fn snapshot_window_redraw(").unwrap() + start];
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
    let invalidate = &redraw[redraw.find("fn invalidate_visibility_frame(").unwrap()
        ..redraw.find("fn request_visible_frame(").unwrap()];
    assert!(invalidate.contains("renderer.invalidate_retained_frame()"));
    assert!(!invalidate.contains(".lock()"));
    let request = &redraw[redraw.find("fn request_visible_frame(").unwrap()
        ..redraw.find("fn surface_probe_deadline(").unwrap()];
    assert!(
        request.contains("!self.redraw.request_in_flight")
            && request.contains("renderer.device_accepts_gpu_work()")
    );
    assert_eq!(request.matches("self.request_window_redraw()").count(), 1);
}

/// Replacing a stopped backend drops only that device's occlusion, never the native window's visibility state.
#[test]
pub(super) fn validated_device_recovery_clears_only_backend_occlusion() {
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
pub(super) fn arm_settled_fade_scrollbar(app: &mut App, owner: WindowId, active: Instant) -> u64 {
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
pub(super) fn scrollbar_due(app: &App, owner: WindowId, now: Instant) -> Vec<Instant> {
    app.frame_due_work_at(now)
        .iter()
        .filter(|work| work.owner == Some(owner) && work.cause == DueCause::Scrollbar)
        .map(|work| work.deadline)
        .collect()
}

#[test]
pub(super) fn an_animated_idle_deadline_is_serviced_once_and_spares_a_later_owner() {
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
pub(super) fn hidden_occluded_and_parked_owners_contribute_no_idle_deadline() {
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
pub(super) fn activity_or_a_hold_after_collection_makes_the_expiry_a_no_op() {
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
pub(super) fn an_overdue_expiry_after_an_earlier_retarget_requests_the_fade_frame() {
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
pub(super) fn idle_release_owner(deadline: Instant) -> (App, WindowId) {
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

pub(super) fn release_work(app: &App, now: Instant) -> Vec<WindowId> {
    app.frame_due_work_at(now)
        .into_iter()
        .filter(|work| work.cause == DueCause::ImageAtlasRelease)
        .filter_map(|work| work.owner)
        .collect()
}

pub(super) fn releases(app: &App, id: WindowId) -> u32 {
    app.windows[&id].test_image_atlas_release.as_ref().unwrap().releases
}

/// A release is collected only for an idle, visible window at or after its deadline; every frame-family
/// blocker (deferred, a request in flight, a pending cause, hidden, occluded, parked) excludes it.
#[test]
pub(super) fn image_atlas_release_requires_an_idle_visible_window() {
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
pub(super) fn an_eligible_release_runs_once_without_a_frame() {
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
pub(super) fn work_pending_between_collection_and_service_defers_the_release() {
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

/// Owners whose App counts frames, so a test can read each window's deferral rule counts.
pub(super) fn counting_owners() -> (App, WindowId, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child", "background"]);
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    (app, main, child)
}

/// Move both of a window's pacing clocks back, so pacing does not depend on when the test built it.
pub(super) fn backdate_clocks(app: &mut App, id: WindowId, at: Instant) {
    let window = app.windows.get_mut(&id).unwrap();
    window.last_render = at;
    window.stream_clock = at;
}

/// Publish one completed output batch on the window's visible active pane.
pub(super) fn publish_visible_output(app: &App, id: WindowId) {
    let window = &app.windows[&id];
    let pane = window.tab_states[window.tabs.active_index()].active_pane;
    window.panes[&pane].output_generation.fetch_add(1, Ordering::Release);
}

/// Run one admitted attempt for `cause` through the production begin, snapshot and finish adapters.
pub(super) fn admitted_attempt(
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
pub(super) fn defer_count(
    app: &App,
    id: WindowId,
    rule: super::super::frame_counters::DeferRule,
) -> u64 {
    use super::super::frame_counters::DeferRule;
    let counters = app.windows[&id].redraw.frame_counters.as_deref().expect("counting App");
    match rule {
        DeferRule::Timeout => counters.defer_timeout,
        DeferRule::Contention => counters.defer_contention,
        DeferRule::Sync => counters.defer_sync,
        DeferRule::Streaming => counters.defer_streaming,
    }
}

/// The text of the method `signature` names, up to the closing brace of an impl-level method.
pub(super) fn method_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let end = source[start..].find("\n    }\n").map_or(source.len(), |offset| start + offset);
    &source[start..end]
}

/// A surface timeout waits one period from its own attempt, even when an exempt keypress came first.
#[test]
pub(super) fn surface_timeout_retry_still_waits_one_period_from_the_attempt() {
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
pub(super) fn unchanged_non_input_passes_stay_one_per_period() {
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
pub(super) fn outcomes_that_advance_the_streaming_clock() {
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
pub(super) fn degraded_software_keeps_its_cadence_after_a_keypress() {
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

/// `frame` counts presented content frames only: output, input and resize count; a strip-only or
/// cursor-blink frame does not; a strip-only attempt that retries and then presents does not; a
/// content attempt that retries counts once, when it presents; cached and settled never count;
/// with recording off nothing is recorded.
#[test]
pub(super) fn only_presented_content_frames_are_recorded() {
    let now = Instant::now();
    let mut state =
        WindowRedrawState { presented_frames: Some((0, 0)), ..WindowRedrawState::default() };
    let attempt = |state: &mut WindowRedrawState, causes: &[RedrawCause], outcome| {
        for cause in causes {
            state.mark(*cause);
        }
        let snapshot = state.snapshot();
        state.settle(snapshot, outcome, now);
        state.presented_frames.map_or(0, |(frames, _)| frames)
    };
    use FrameSettlement::{Cached, Presented, Settled};
    assert_eq!(attempt(&mut state, &[RedrawCause::Output], Presented), 1);
    assert_eq!(attempt(&mut state, &[RedrawCause::Input], Presented), 2);
    assert_eq!(attempt(&mut state, &[RedrawCause::Topology], Presented), 3);
    assert_eq!(attempt(&mut state, &[RedrawCause::StatusLine], Presented), 3, "strip only");
    assert_eq!(attempt(&mut state, &[RedrawCause::Cursor], Presented), 3, "blink only");
    let surface = FrameSettlement::SurfaceRetry(SurfaceRetryReason::Outdated);
    assert_eq!(attempt(&mut state, &[RedrawCause::StatusLine], surface), 3);
    assert_eq!(attempt(&mut state, &[], Presented), 3, "the strip's retry is not content");
    let atlas = FrameSettlement::Retry(RedrawCause::AtlasRetry);
    assert_eq!(attempt(&mut state, &[RedrawCause::Input], atlas), 3);
    assert_eq!(attempt(&mut state, &[], Presented), 4, "the content attempt counts once");
    assert_eq!(attempt(&mut state, &[], Presented), 4);
    assert_eq!(attempt(&mut state, &[RedrawCause::Output], Cached), 4);
    assert_eq!(attempt(&mut state, &[RedrawCause::Output], Settled), 4);
    let mut off = WindowRedrawState::default();
    assert_eq!(attempt(&mut off, &[RedrawCause::Output], Presented), 0);
    assert_eq!(off.presented_frames, None);
}

/// A frame whose only unsettled cause is `StatusLine` sets no foreground-probe demand, so a
/// probe result can never repaint the strip into another frame; any other cause, a forced
/// resize or a burst changes that. Both frame passes route through the gate.
#[test]
pub(super) fn a_status_only_frame_schedules_no_foreground_probe() {
    let mut strip = WindowRedrawState::default();
    strip.mark(RedrawCause::StatusLine);
    assert!(strip.status_only_attempt());
    assert!(!sets_fg_demand(false, strip.status_only_attempt()), "strip only: no probe");
    let mut output = WindowRedrawState::default();
    output.mark(RedrawCause::StatusLine);
    output.mark(RedrawCause::Output);
    assert!(!output.status_only_attempt());
    assert!(sets_fg_demand(false, output.status_only_attempt()), "content sets demand");
    let mut resized = WindowRedrawState { resize_pending: true, ..WindowRedrawState::default() };
    resized.mark(RedrawCause::StatusLine);
    assert!(!resized.status_only_attempt(), "a forced resize frame is not strip-only");
    assert!(!WindowRedrawState::default().status_only_attempt(), "nothing pending");
    assert!(!sets_fg_demand(true, false), "a burst never probes");
    for source in [
        concat!(
            include_str!("window_event.rs"),
            "\n",
            include_str!("window_event_pointer.rs"),
            "\n",
            include_str!("window_event_redraw.rs")
        ),
        include_str!("child_window_redraw.rs"),
    ] {
        assert!(source.contains("super::redraw::sets_fg_demand(pty_burst, status_only)"));
        assert!(!source.contains("(!pty_burst).then(|| std::sync::Arc::clone(&self.fg_probes))"));
    }
}
