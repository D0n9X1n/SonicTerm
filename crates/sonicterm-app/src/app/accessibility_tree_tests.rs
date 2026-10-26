//! Accessibility tree structure, localized health and announcement contracts.

use super::super::accessibility_tests::*;
use super::*;

#[test]
fn terminal_windows_carry_a_dormant_adapter_and_build_nothing_before_activation() {
    // Every terminal window has state under its id; with no assistive technology connected a
    // committed strip, a sample and a flush build no tree and retain no values.
    let (mut app, window_id) = app_with_window();
    let main = app.main_window_id.expect("the seed made a main window");
    assert!(app.accessibility.contains_key(&main));
    assert!(!access(&app, window_id).has_adapter(), "headless windows have no platform adapter");
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    let _ = app.sync_status_line(Instant::now());
    let _ = app.flush_accessibility(Instant::now());
    assert_eq!(app.accessibility_tree_builds, 0);
    assert!(access(&app, window_id).sent.is_empty());
    assert_eq!(access(&app, window_id).retained_items(), 0, "a dormant window keeps nothing");
}

#[test]
fn every_terminal_creation_path_installs_one_adapter_before_first_show() {
    // The adapter must exist before a window is shown: each path creates its window hidden,
    // installs the adapter under its id, and only later shows it. A spare keeps its entry through
    // tear-out, and the Memory Usage report window gets none.
    let install = "self.install_accessibility(event_loop, &window)";
    for (source, function, show) in [
        (
            include_str!("event_loop.rs"),
            "pub(super) fn do_resumed(",
            Some("window.set_visible(true)"),
        ),
        (
            concat!(
                include_str!("misc.rs"),
                "\n",
                include_str!("misc_clipboard.rs"),
                "\n",
                include_str!("misc_window_create.rs")
            ),
            "fn create_new_terminal_window(",
            Some("window.set_visible(true)"),
        ),
        (
            concat!(
                include_str!("tear_out.rs"),
                "\n",
                include_str!("tear_out_destination.rs"),
                "\n",
                include_str!("tear_out_warm.rs")
            ),
            "fn create_warm_window(",
            None,
        ),
        (
            concat!(
                include_str!("tear_out.rs"),
                "\n",
                include_str!("tear_out_destination.rs"),
                "\n",
                include_str!("tear_out_warm.rs")
            ),
            "fn prepare_tear_out_destination(",
            None,
        ),
    ] {
        let text = body(source, function);
        assert_eq!(text.matches(install).count(), 1, "{function} installs one adapter");
        let created = text.find("create_window(").expect("the path creates a window");
        let installed = text.find(install).unwrap();
        assert!(created < installed, "{function}: the window exists before the adapter");
        if let Some(show) = show {
            assert!(installed < text.find(show).unwrap(), "{function}: installed before shown");
        }
    }
    let report = include_str!("memory_report_window.rs");
    assert!(!report.contains("install_accessibility"));
}

#[test]
fn the_tree_has_stable_ids_buttons_for_clicks_and_a_persistent_announcement() {
    // Root, strip group and segments in visual order; clickable segments are buttons with
    // `Click`, inert ones labels with bounds; everything is live off except the announcement,
    // which is exposed and present; a reorder keeps every segment's id.
    let (mut app, window_id) = app_with_window();
    present(
        &mut app,
        window_id,
        &[(StatusSegmentId::Mem, 100.0, true), (StatusSegmentId::Scroll, 0.0, false)],
    );
    activate(&mut app, window_id, Instant::now());
    let tree = last_tree(&app, window_id);
    assert_eq!(tree.tree.as_ref().unwrap().root, ROOT_NODE);
    assert_eq!(node(&tree, ROOT_NODE).unwrap().children(), [STRIP_NODE, ANNOUNCEMENT_NODE]);
    let strip = node(&tree, STRIP_NODE).unwrap();
    assert_eq!(
        (strip.role(), strip.label(), strip.live()),
        (Role::Group, Some("Status line"), Some(Live::Off))
    );
    let scroll = segment_node(StatusSegmentId::Scroll);
    let mem = segment_node(StatusSegmentId::Mem);
    assert_eq!(strip.children(), [scroll, mem], "visual order");
    let mem_node = node(&tree, mem).unwrap();
    assert_eq!(mem_node.role(), Role::Button);
    assert!(mem_node.supports_action(AccessAction::Click));
    assert!(mem_node.label().is_some_and(|label| label.starts_with("Memory")));
    let scroll_node = node(&tree, scroll).unwrap();
    assert_eq!(scroll_node.role(), Role::Label);
    assert!(!scroll_node.supports_action(AccessAction::Click));
    assert!(scroll_node.bounds().is_some(), "inert segments still carry bounds");
    assert_eq!(scroll_node.live(), Some(Live::Off));
    let announcement = node(&tree, ANNOUNCEMENT_NODE).unwrap();
    assert!(!announcement.is_hidden());
    assert_eq!(announcement.live(), Some(Live::Polite));
    present(
        &mut app,
        window_id,
        &[(StatusSegmentId::Mem, 0.0, true), (StatusSegmentId::Scroll, 100.0, false)],
    );
    let _ = app.flush_accessibility(Instant::now());
    let reordered = last_tree(&app, window_id);
    assert_eq!(node(&reordered, STRIP_NODE).unwrap().children(), [mem, scroll]);
}

#[test]
fn no_redraw_path_builds_a_tree() {
    // Finding #4: the tree is built in one App step after dispatch, never on the main or child
    // redraw path, where accesskit's Unix adapter would take a blocking mutex.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let sent = access(&app, window_id).sent.len();
    app.observe_status_pane(window_id, Default::default());
    assert_eq!(access(&app, window_id).sent.len(), sent, "the redraw's observation sent nothing");
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
        assert!(!source.contains("flush_accessibility"));
        assert!(!source.contains("flush_window_accessibility"));
    }
    assert!(!body(
        concat!(
            include_str!("status_line.rs"),
            "\n",
            include_str!("status_line_actions.rs"),
            "\n",
            include_str!("status_line_inputs.rs"),
            "\n",
            include_str!("status_line_sync.rs")
        ),
        "pub(super) fn observe_status_pane("
    )
    .contains("accessibility("));
    assert!(body(include_str!("event_loop.rs"), "pub(super) fn do_about_to_wait(")
        .contains("self.flush_accessibility(now)"));
}

#[test]
fn a_collection_that_presents_nothing_keeps_the_committed_strip() {
    // Finding #6: an observation from a frame that presented nothing (a contended collection,
    // or no renderer at all) leaves the committed layout, so the tree's structure stays.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    app.observe_status_pane(window_id, Default::default());
    assert!(app.windows[&window_id].status_strip.is_some(), "the committed layout is kept");
}

#[test]
fn memory_is_sampled_and_spoken_while_no_window_can_draw() {
    // Finding #14: with every window unable to draw (no renderer here; covered, parked or
    // device-stopped in production), a connected reader still hears memory from a real sampler
    // tick, without any frame.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let name = |app: &App| {
        node(&last_tree(app, window_id), segment_node(StatusSegmentId::Mem))
            .and_then(|node| node.label().map(str::to_owned))
            .unwrap_or_default()
    };
    let _ = app.sync_status_line(Instant::now());
    let _ = app.flush_accessibility(Instant::now());
    assert!(
        name(&app).chars().any(|character| character.is_ascii_digit()),
        "the sampled figure is read: {}",
        name(&app)
    );
}

#[test]
fn health_announces_while_the_strip_is_off() {
    // Findings #5 and #13: announcements come from the inputs, never from the strip being on or
    // a segment being in a visual list.
    let (mut app, window_id) = app_with_window();
    app.toggle_status_line();
    let _ = app.sync_status_line(Instant::now());
    activate(&mut app, window_id, Instant::now());
    app.set_config_reload_error(Some(&anyhow::anyhow!("bad key")));
    let _ = app.flush_accessibility(Instant::now());
    let said = announced(&app, window_id).expect("health announced with the strip off");
    assert!(said.text.starts_with("Error:") && said.assertive, "{}", said.text);
    let tree = last_tree(&app, window_id);
    assert!(node(&tree, STRIP_NODE).is_none(), "no segment is exposed while the strip is off");
}

#[test]
fn the_baseline_is_taken_when_a_reader_connects() {
    // Finding #10: what is already wrong when a reader connects is the baseline, read from the
    // tree and never announced.
    let (mut app, window_id) = app_with_window();
    app.set_config_reload_error(Some(&anyhow::anyhow!("bad key")));
    activate(&mut app, window_id, Instant::now());
    let _ = app.flush_accessibility(Instant::now());
    assert_eq!(announced(&app, window_id), None);
}

#[test]
fn a_rate_limited_recovery_is_delivered_once_its_interval_passes() {
    // Finding #11: a transition inside the minimum interval waits as the segment's latest
    // pending one and is delivered when the interval passes, so a recovery is never lost.
    let (mut app, window_id) = app_with_window();
    let start = Instant::now();
    activate(&mut app, window_id, start);
    app.set_config_reload_error(Some(&anyhow::anyhow!("bad key")));
    let _ = app.flush_accessibility(start + Duration::from_secs(1));
    assert!(announced(&app, window_id).unwrap().text.starts_with("Error:"));
    app.set_config_reload_error(None);
    let wake = app.flush_accessibility(start + Duration::from_secs(2));
    assert!(announced(&app, window_id).unwrap().text.starts_with("Error:"), "still waiting");
    assert_eq!(wake, Some(start + Duration::from_secs(6)), "a wake for the interval's end");
    let _ = app.flush_accessibility(start + Duration::from_secs(6));
    assert_eq!(announced(&app, window_id).unwrap().text, "Health recovered");
}

#[test]
fn an_assertive_announcement_is_never_overwritten_by_a_polite_one() {
    // Finding #12: queued announcements are delivered one per tree update, assertive first,
    // with the polite one following after the spacing.
    let (mut app, window_id) = app_with_window();
    let start = Instant::now();
    activate(&mut app, window_id, start);
    let _ = app.sync_status_line(start);
    // Tab 2 is active in the seeded window, and an active tab takes no mark.
    app.windows.get_mut(&window_id).unwrap().tabs.mark(0, |marks| marks.bell = true);
    app.set_config_reload_error(Some(&anyhow::anyhow!("bad key")));
    let wake = app.flush_accessibility(start + Duration::from_secs(1));
    let first = announced(&app, window_id).unwrap();
    assert!(first.assertive && first.text.starts_with("Error:"), "{}", first.text);
    assert_eq!(wake, Some(start + Duration::from_secs(1) + ANNOUNCE_SPACING));
    let _ = app.flush_accessibility(start + Duration::from_secs(1) + ANNOUNCE_SPACING);
    let second = announced(&app, window_id).unwrap();
    assert!(!second.assertive && second.text.contains("bell"), "{}", second.text);
}

#[test]
fn a_request_resolved_before_its_pane_was_replaced_is_dropped() {
    // Round 4 item 1: a request runs only while the target it resolved (segment, pane, action,
    // reload and resize epoch) is what the window exposes; a pane change since drops it, and a
    // request resolved against the new tree runs.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let mem = segment_node(StatusSegmentId::Mem).0;
    let target = access(&app, window_id).resolve(mem);
    assert!(target.is_some(), "the handler resolves the node against the published targets");
    app.windows.get_mut(&window_id).unwrap().tabs.activate(0);
    let _ = app.flush_accessibility(Instant::now());
    app.handle_accessibility_request(window_id, AccessibilityRequest::Click { node: mem, target });
    assert!(app.status_line_shown(), "the click resolved before the pane change is dropped");
    let target = access(&app, window_id).resolve(mem);
    app.handle_accessibility_request(window_id, AccessibilityRequest::Click { node: mem, target });
    assert!(!app.status_line_shown(), "a click resolved against the current tree runs");
}

#[test]
fn a_request_runs_across_a_content_only_tree_update() {
    // Round 4 item 1: a tree update that changes only what is read (here the announcement) keeps
    // the target, so a request resolved before it still runs.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    let start = Instant::now();
    activate(&mut app, window_id, start);
    let mem = segment_node(StatusSegmentId::Mem).0;
    let target = access(&app, window_id).resolve(mem);
    let sent = access(&app, window_id).sent.len();
    app.set_config_reload_error(Some(&anyhow::anyhow!("bad key")));
    let _ = app.flush_accessibility(start + Duration::from_secs(1));
    assert!(access(&app, window_id).sent.len() > sent, "a content-only tree was sent");
    app.handle_accessibility_request(window_id, AccessibilityRequest::Click { node: mem, target });
    assert!(!app.status_line_shown(), "the unchanged target runs");
}

#[test]
fn a_click_after_exposure_is_turned_off_is_dropped() {
    // Finding #2: a node that `expose = false` removed cannot be clicked, even with the target it
    // resolved while the tree still had it.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let mem = segment_node(StatusSegmentId::Mem).0;
    let target = access(&app, window_id).resolve(mem);
    app.config.status_line.accessibility.expose = false;
    let _ = app.sync_status_line(Instant::now());
    app.handle_accessibility_request(window_id, AccessibilityRequest::Click { node: mem, target });
    assert!(app.status_line_shown(), "the click was dropped");
    let _ = app.flush_accessibility(Instant::now());
    let hidden = last_tree(&app, window_id);
    assert!(node(&hidden, STRIP_NODE).is_none());
    assert_eq!(node(&hidden, ROOT_NODE).unwrap().children(), [ANNOUNCEMENT_NODE]);
}

#[test]
fn a_reader_that_reaches_a_warm_spare_is_served_on_promotion() {
    // Finding #3: state is kept per window id, a spare's included, and its events reach the
    // adapter before the warm return; an initial-tree request made before promotion is
    // recorded and served by the first flush after it.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    let promoted = app.windows.remove(&window_id).expect("the seeded window");
    app.handle_accessibility_request(window_id, AccessibilityRequest::InitialTree);
    assert!(access(&app, window_id).active, "the request is recorded on the spare's entry");
    assert!(access(&app, window_id).sent.is_empty());
    app.windows.insert(window_id, promoted);
    let _ = app.flush_accessibility(Instant::now());
    assert!(node(&last_tree(&app, window_id), STRIP_NODE).is_some(), "served on promotion");
    let events = body(
        concat!(
            include_str!("window_event.rs"),
            "\n",
            include_str!("window_event_pointer.rs"),
            "\n",
            include_str!("window_event_redraw.rs")
        ),
        "pub(super) fn do_window_event(",
    );
    assert!(
        events.find("self.forward_accessibility_event(win_id, &event)").unwrap()
            < events.find("self.is_warm_window_id(win_id)").unwrap()
    );
}

#[test]
fn a_held_key_never_reaches_the_terminal_while_the_strip_has_focus() {
    // Finding #7: the strip owns keys before the retained-repeat route, and a repeat it does not
    // navigate with is consumed without changing focus.
    let (mut app, window_id) = app_with_window();
    present(
        &mut app,
        window_id,
        &[(StatusSegmentId::Mem, 0.0, true), (StatusSegmentId::Cpu, 50.0, false)],
    );
    app.focus_status_line(window_id);
    assert!(app.status_focus_key(window_id, &Key::Character(SmolStr::new("a")), true));
    assert_eq!(access(&app, window_id).focus, Some(StatusSegmentId::Mem), "kept");
    assert!(app.status_focus_key(window_id, &Key::Named(NamedKey::ArrowRight), true));
    assert_eq!(access(&app, window_id).focus, Some(StatusSegmentId::Cpu), "repeats navigate");
    let keyboard = body(
        concat!(include_str!("window_keyboard.rs"), "\n", include_str!("window_keyboard_ime.rs")),
        "pub(super) fn handle_window_keyboard(",
    );
    assert!(
        keyboard.find("self.status_focus_key(").unwrap()
            < keyboard.find("terminal_repeat_targets(").unwrap()
    );
}

#[test]
fn taking_strip_focus_ends_an_active_composition() {
    // Finding #8: the strip takes the window's keys, so a composition in progress ends with its
    // preedit.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    assert!(app.__test_set_child_ime_preedit(window_id, "か"));
    assert_eq!(app.__test_child_ime_composing(window_id), Some(true));
    app.focus_status_line(window_id);
    assert_eq!(app.__test_child_ime_composing(window_id), Some(false));
}

#[test]
fn any_press_outside_the_strip_ends_strip_focus_first() {
    // Finding #9: a press of any button outside the strip ends strip focus before a field, the
    // palette, a toast or a pane handles it; a press inside keeps it.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
        app.focus_status_line(window_id);
        app.windows.get_mut(&window_id).unwrap().cursor_pos = (10.0, 510.0);
        let _ = app.route_shared_pointer(window_id, &press(MouseButton::Middle));
        assert_eq!(access(&app, window_id).focus, Some(StatusSegmentId::Mem), "inside keeps it");
        app.windows.get_mut(&window_id).unwrap().cursor_pos = (10.0, 100.0);
        let _ = app.route_shared_pointer(window_id, &press(button));
        assert_eq!(access(&app, window_id).focus, None, "{button:?} outside ends it");
    }
    let shared = body(
        concat!(
            include_str!("window_event.rs"),
            "\n",
            include_str!("window_event_pointer.rs"),
            "\n",
            include_str!("window_event_redraw.rs")
        ),
        "fn route_shared_pointer(",
    );
    assert!(
        shared.find("self.end_status_focus_outside(win_id)").unwrap()
            < shared.find("self.field_pointer_event(").unwrap()
    );
    let events = concat!(
        include_str!("window_event.rs"),
        "\n",
        include_str!("window_event_pointer.rs"),
        "\n",
        include_str!("window_event_redraw.rs")
    );
    assert!(
        events.find("self.route_shared_pointer(win_id, &event)").unwrap()
            < events.find("self.handle_main_left_mouse_input(").unwrap()
    );
}

#[test]
fn a_reload_that_changes_nothing_read_sends_no_tree() {
    // Finding #15: the content hash covers names, roles, bounds, actions, focus and the
    // announcement, never the strip settings generation.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    // A connected reader makes the first pass sample memory, a real value change; settle it.
    let _ = app.sync_status_line(Instant::now());
    let _ = app.flush_accessibility(Instant::now());
    let sent = access(&app, window_id).sent.len();
    app.invalidate_status_line();
    let _ = app.sync_status_line(Instant::now());
    let _ = app.flush_accessibility(Instant::now());
    assert_eq!(access(&app, window_id).sent.len(), sent);
}

#[test]
fn a_reload_applies_strong_focus_to_a_drawn_ring() {
    // Finding #16: `strong_focus` reaches the focused segment's ring on reload, not only on the
    // next focus move.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    app.focus_status_line(window_id);
    assert_eq!(access(&app, window_id).ring, Some((StatusSegmentId::Mem, false)));
    let mut config = app.config.clone();
    config.accessibility.strong_focus = true;
    app.apply_new_config(config);
    assert_eq!(access(&app, window_id).ring, Some((StatusSegmentId::Mem, true)));
}

#[test]
fn the_retained_model_is_charged_to_the_window_owner_and_stays_bounded() {
    // Finding #17: a connected window's model is charged to its window owner and released on
    // disconnect; however many transitions arrive, it keeps at most one per segment.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    let owner = app.windows[&window_id].owner.as_ref().map(|guard| guard.id()).expect("owner");
    let charged = |app: &App| {
        app.__test_owner_snapshot(owner).unwrap().owner_class_bytes
            [ResourceClass::AccessibilityTree]
    };
    let start = Instant::now();
    activate(&mut app, window_id, start);
    assert!(charged(&app) > 0, "the model is in the ledger under the window");
    for step in 1..200_u64 {
        let error = (step % 2 == 0).then(|| anyhow::anyhow!("bad key {step}"));
        app.set_config_reload_error(error.as_ref());
        let _ = app.flush_accessibility(start + Duration::from_millis(step * 10));
    }
    let state = access(&app, window_id);
    assert!(state.queue.len() <= StatusSegmentId::ALL.len());
    assert!(state.announced.len() <= StatusSegmentId::ALL.len());
    app.handle_accessibility_request(window_id, AccessibilityRequest::Deactivated);
    assert_eq!(charged(&app), 0, "nothing is retained for a disconnected reader");
}
