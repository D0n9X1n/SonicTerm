//! Scrollbar window interaction and bounded redraw contracts.

use super::super::scrollbar_visibility_tests::*;
use super::*;

#[test]
fn focus_loss_during_a_drag_and_a_drag_cancel_each_start_a_fade() {
    // Both paths end a drag without a button release; the bar was last active
    // two seconds ago, so ending the hold must start its fade, not leave it shown.
    for cancel in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.config.appearance.scrollbar = ScrollbarMode::Auto;
        let pane = app.__test_seed_tab("drag");
        let main = app.main_window_id.expect("seeded main window");
        let started = at(2, Instant::now());
        let window = app.windows.get_mut(&main).unwrap();
        window.scrollbar_drag = Some(drag_state(pane));
        let mut state = ScrollbarVisState::new(started);
        note_activity(&mut state, started);
        retarget(&mut state, ScrollbarMode::Auto, true, started);
        state.alpha = 1.0;
        window.scrollbar_vis.insert(pane, state);
        if cancel {
            app.cancel_drag_session();
        } else {
            app.handle_window_focus_changed(main, false);
        }
        let window = &app.windows[&main];
        assert!(window.scrollbar_drag.is_none(), "cancel {cancel}");
        let state = window.scrollbar_vis[&pane];
        assert_eq!(state.target, 0.0, "cancel {cancel}: the hold ended");
        assert!(state.transition_start.is_some_and(|start| start > started));
        assert_eq!(state.alpha, 1.0, "cancel {cancel}: an animated bar fades, it does not snap");
        assert!(is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated));
    }
}

#[test]
fn snap_mode_and_the_software_path_still_snap() {
    // Degraded presentation keeps assigning targets at once: shown on activity,
    // hidden in one step at the idle deadline, and the deadline is consumed.
    assert_eq!(window_scrollbar_motion(None, true), ScrollbarMotion::Snap);
    let active = Instant::now();
    let mut state = ScrollbarVisState::new(active);
    note_activity(&mut state, active);
    retarget(&mut state, ScrollbarMode::Auto, false, active);
    let shown = active + Duration::from_millis(1);
    assert_eq!(tick(&mut state, ScrollbarMode::Auto, false, ScrollbarMotion::Snap, shown), 1.0);
    let deadline = active + IDLE;
    let mut vis = HashMap::from([(1, state)]);
    assert!(expire_due_idle(&mut vis, ScrollbarMode::Auto, None, ScrollbarMotion::Snap, deadline));
    assert_eq!(vis[&1].alpha, 0.0);
    assert!(vis[&1].idle_consumed);
    assert!(!is_animating(&vis[&1], ScrollbarMode::Auto, ScrollbarMotion::Snap));
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
}

#[test]
fn a_held_scrollbar_arms_no_idle_deadline() {
    // Edge hover and a drag hold the bar visible, so neither contributes a
    // deadline and expiry leaves the held bar alone.
    let active = Instant::now();
    let later = active + Duration::from_secs(10);
    let mut vis = HashMap::from([(1, settled_visible(active))]);
    vis.get_mut(&1).unwrap().mouse_near_right_edge = true;
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
    assert!(!expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Animated,
        later
    ));
    vis.get_mut(&1).unwrap().mouse_near_right_edge = false;
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, Some(1)), None);
    assert!(!expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        Some(1),
        ScrollbarMotion::Animated,
        later
    ));
    let state = vis[&1];
    assert!(!state.idle_consumed);
    assert_eq!((state.alpha, state.target), (1.0, 1.0));
}

#[test]
fn two_hover_show_hide_cycles_each_end_hidden() {
    // The first hover is held past the idle window; the second leaves 200 ms
    // after entry, so only its deadline can hide it. Each entry arms a deadline
    // and each cycle comes to rest hidden with no deadline left.
    let mut vis = HashMap::new();
    let mut now = Instant::now();
    for hover_ms in [700u64, 200] {
        assert!(update_hover_states(&mut vis, &[PANE], NEAR_EDGE, ScrollbarMode::Auto, None, now));
        let entry = now;
        assert_eq!(vis[&1].last_active, Some(entry));
        assert!(!vis[&1].idle_consumed, "hover {hover_ms}: entry arms a deadline");
        now = drive_to_rest(&mut vis, NEAR_EDGE, now);
        assert_eq!(vis[&1].alpha, 1.0, "hover {hover_ms}: shown while held");
        now = now.max(entry + Duration::from_millis(hover_ms));
        assert!(update_hover_states(&mut vis, &[PANE], AWAY, ScrollbarMode::Auto, None, now));
        assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), Some(entry + IDLE));
        now = drive_to_rest(&mut vis, AWAY, now);
        let state = vis[&1];
        assert_eq!(state.alpha, 0.0, "hover {hover_ms}: the cycle ends hidden");
        assert!(state.idle_consumed);
        assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
        now += Duration::from_secs(1);
    }
}

#[test]
fn every_scrollbar_target_input_write_is_followed_by_a_retarget() {
    // The target is stored, not recomputed per frame, so a write to a target
    // input (drag, edge hover, activity) that skips `retarget` leaves the
    // stored target stale: a release that never fades, or a deadline that
    // never arms. Every such write must be followed by a retarget in the same
    // function, and the inventory pins every site so a new one is reviewed.
    const PATTERNS: [&str; 5] = [
        "scrollbar_drag = ",
        "scrollbar_drag.take()",
        "mouse_near_right_edge = ",
        "last_active = ",
        "note_activity(",
    ];
    let mut found: Vec<(String, &str, usize)> = Vec::new();
    for (name, text) in production_sources() {
        let lines: Vec<&str> = text.lines().collect();
        for pattern in PATTERNS {
            let mut site_count = 0;
            for (index, line) in lines.iter().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//")
                    || !line.contains(pattern)
                    || line.contains("fn note_activity(")
                {
                    continue;
                }
                let opener = lines[..index].iter().rev().find(|prior| opens_function(prior));
                if pattern == "last_active = "
                    && opener.is_some_and(|open| open.contains("fn note_activity("))
                {
                    // `note_activity` is the activity operation itself.
                    continue;
                }
                let end = lines[index + 1..]
                    .iter()
                    .position(|next| opens_function(next))
                    .map_or(lines.len(), |offset| index + 1 + offset);
                let rest = lines[index..end].join("\n");
                assert!(
                    ["retarget(", "retarget_panes(", "retarget_scrollbars("]
                        .iter()
                        .any(|call| rest.contains(call)),
                    "{name}:{}: `{pattern}` is not followed by a retarget",
                    index + 1
                );
                site_count += 1;
            }
            if site_count > 0 {
                found.push((name.clone(), pattern, site_count));
            }
        }
    }
    found.sort();
    let expected: Vec<(String, &str, usize)> = vec![
        ("app/scrollbar_visibility.rs".into(), "mouse_near_right_edge = ", 3),
        ("app/scrollbar_visibility.rs".into(), "note_activity(", 3),
        ("app/scrollbar_visibility_window.rs".into(), "scrollbar_drag = ", 1),
        ("app/scrollbar_visibility_window.rs".into(), "scrollbar_drag.take()", 1),
    ];
    assert_eq!(found, expected);
}

#[test]
fn releasing_a_settled_drag_over_text_requests_the_fade_frame_in_main_and_child() {
    // The reviewer's sequence: a drag held past the idle window is released
    // over plain text with nothing else pending. The release retargets the bar
    // to hidden while it is still drawn, so it must ask its own window for a
    // frame, or the bar stays shown. Both release handlers end the drag through
    // `end_scrollbar_drag`; the handlers themselves need a native event loop.
    for child in [false, true] {
        let (mut app, window_id, pane, before) = settled_drag(child);
        let release = Instant::now();
        assert!(app
            .windows
            .get_mut(&window_id)
            .unwrap()
            .end_scrollbar_drag(ScrollbarMode::Auto, release));
        let window = &app.windows[&window_id];
        let state = window.scrollbar_vis[&pane];
        assert_eq!((state.alpha, state.target), (1.0, 0.0), "child {child}");
        assert!(window.redraw.request_in_flight, "child {child}: the release requested a frame");
        assert_ne!(window.redraw.snapshot(), before, "child {child}: a scrollbar cause is marked");
        frames_until_settled(&mut app, window_id, pane, release + FRAME);
        assert_eq!(app.windows[&window_id].scrollbar_vis[&pane].alpha, 0.0, "child {child}");
    }
    for handler in [include_str!("window_pointer.rs"), include_str!("child_window_pointer.rs")] {
        assert!(handler.replace("\r\n", "\n").contains(".end_scrollbar_drag("));
    }
}

#[test]
fn focus_loss_and_a_drag_cancel_request_the_fade_frame() {
    // Both end a settled drag without a release; each must wake the window.
    for cancel in [false, true] {
        let (mut app, window_id, pane, before) = settled_drag(false);
        if cancel {
            app.cancel_drag_session();
        } else {
            app.handle_window_focus_changed(window_id, false);
        }
        let window = &app.windows[&window_id];
        assert!(window.scrollbar_drag.is_none(), "cancel {cancel}");
        assert_eq!(window.scrollbar_vis[&pane].target, 0.0, "cancel {cancel}");
        assert!(window.redraw.request_in_flight, "cancel {cancel}: a frame was requested");
        assert_ne!(window.redraw.snapshot(), before, "cancel {cancel}");
    }
}

#[test]
fn an_overdue_expiry_reports_a_pane_still_fading_toward_an_earlier_target() {
    // The target already went to 0 (by a release or a frame) but no frame
    // ran. The overdue deadline must still report the pane as needing a frame.
    let active = Instant::now();
    let mut state = settled_visible(active);
    let late = active + IDLE + Duration::from_millis(200);
    assert!(retarget(&mut state, ScrollbarMode::Auto, false, late));
    let mut vis = HashMap::from([(1, state)]);
    assert!(expire_due_idle(&mut vis, ScrollbarMode::Auto, None, ScrollbarMotion::Animated, late));
    assert!(vis[&1].idle_consumed);
    assert!(!expire_due_idle(&mut vis, ScrollbarMode::Auto, None, ScrollbarMotion::Animated, late));
}

#[test]
fn every_scrollbar_retarget_caller_wakes_its_window() {
    // A retarget can leave alpha short of its target with no frame coming.
    // The pure helpers that retarget may be called only from other pure helpers
    // in this module, from window methods that then call
    // `wake_scrollbar_if_animating`, or from the three paths that request their
    // own frame: idle expiry (the deadline service) and the two render paths.
    const RETARGETING: [&str; 7] = [
        "retarget(",
        "retarget_panes(",
        "note_pane_activity(",
        "update_hover_states(",
        "clear_hover_states(",
        "expire_due_idle(",
        "update_and_collect(",
    ];
    const OWN_FRAME: [(&str, &str); 3] = [
        ("app/event_loop_wait.rs", "expire_due_idle("),
        ("app/window_event.rs", "update_and_collect("),
        ("app/child_window_redraw.rs", "update_and_collect("),
    ];
    let mut method_sites = Vec::new();
    for (name, text) in production_sources() {
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || line.contains("fn ") {
                continue;
            }
            for callee in RETARGETING {
                let hit = line.match_indices(callee).any(|(at, _)| {
                    !line[..at].ends_with(|character: char| {
                        character.is_alphanumeric() || character == '_'
                    })
                });
                if !hit {
                    continue;
                }
                let opener_at = lines[..index].iter().rposition(|prior| opens_function(prior));
                let opener = opener_at.map_or("", |at| lines[at]);
                let free_function = !opener.starts_with(' ');
                if OWN_FRAME.contains(&(name.as_str(), callee)) {
                    continue;
                }
                if name == "app/scrollbar_visibility.rs" && free_function {
                    continue;
                }
                assert!(!free_function, "{name}:{}: `{callee}` outside a window method", index + 1);
                let end = lines[index + 1..]
                    .iter()
                    .position(|next| opens_function(next))
                    .map_or(lines.len(), |offset| index + 1 + offset);
                let body = lines[opener_at.unwrap()..end].join("\n");
                assert!(
                    body.contains("wake_scrollbar_if_animating()"),
                    "{name}:{}: `{callee}` in a method that never wakes its window",
                    index + 1
                );
                method_sites.push(format!("{name}:{callee}"));
            }
        }
    }
    method_sites.sort();
    assert_eq!(
        method_sites,
        [
            "app/scrollbar_visibility_window.rs:clear_hover_states(",
            "app/scrollbar_visibility_window.rs:note_pane_activity(",
            "app/scrollbar_visibility_window.rs:retarget_panes(",
            "app/scrollbar_visibility_window.rs:update_hover_states(",
        ]
    );
}

#[test]
fn crossing_the_edge_band_asks_for_one_redraw_each_way_in_main_and_child() {
    // The redraw-request counters measure this path, so an edge crossing must
    // ask once, through the window's scrollbar wake, not once more through a
    // second request. Leaving after the idle window starts the fade: one ask.
    // Leaving within it keeps the bar shown, draws nothing new, and asks nothing.
    for child in [false, true] {
        let (mut app, window_id, pane, rect) = edge_band_app(child);
        let row_y = rect.y + rect.h / 2.0;
        let edge = (rect.x + rect.w - 5.0, row_y);
        let away = (rect.x + rect.w / 2.0, row_y);
        assert_eq!(hover_to(&mut app, window_id, child, away), (0, false), "child {child}");

        assert_eq!(hover_to(&mut app, window_id, child, edge), (1, true), "child {child}: entry");
        present(&mut app, window_id, pane);
        let state = app.windows.get_mut(&window_id).unwrap().scrollbar_vis.get_mut(&pane).unwrap();
        state.last_active = Some(at(2, Instant::now()));
        assert_eq!(hover_to(&mut app, window_id, child, away), (1, true), "child {child}: exit");
        assert_eq!(app.windows[&window_id].scrollbar_vis[&pane].target, 0.0);

        present(&mut app, window_id, pane);
        assert_eq!(
            hover_to(&mut app, window_id, child, edge),
            (1, true),
            "child {child}: re-entry"
        );
        present(&mut app, window_id, pane);
        assert_eq!(
            hover_to(&mut app, window_id, child, away),
            (0, true),
            "child {child}: an exit within the idle window changes nothing drawn"
        );
    }
}
