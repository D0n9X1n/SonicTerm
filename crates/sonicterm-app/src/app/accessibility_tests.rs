use super::*;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::status_strip::{PresentedStatusSegment, StripHit};
use sonicterm_types::Action;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::tabs::TabMarks;
use winit::event::{DeviceId, ElementState, MouseButton};
use winit::keyboard::SmolStr;

/// A headless App with the strip on and one torn-out terminal window, which is returned. It has
/// no renderer, so no window is eligible to draw: covered, parked and device-stopped look the same.
fn app_with_window() -> (App, WindowId) {
    let mut config = Config::default();
    config.status_line.enabled = true;
    let mut app = App::new(Theme::default(), config, Keymap::default());
    let window_id = app.__test_seed_child_window(&["first", "second"]);
    let _ = app.sync_status_line(Instant::now());
    (app, window_id)
}

/// Commit a presented strip for `window_id` with `segments` as `(id, left px, click)`, as a frame
/// that drew them would.
fn present_with(
    app: &mut App,
    window_id: WindowId,
    segments: &[(StatusSegmentId, f32, Option<SegmentClick>)],
) {
    let segments = segments
        .iter()
        .map(|(id, left_px, click)| PresentedStatusSegment {
            id: *id,
            x: *left_px,
            width: 40.0,
            click: click.clone(),
        })
        .collect();
    app.windows.get_mut(&window_id).unwrap().status_strip =
        Some(StripHit { top: 500.0, height: 20.0, segments });
}

/// `present_with` where a clickable segment toggles the strip, so a click is observable.
fn present(app: &mut App, window_id: WindowId, segments: &[(StatusSegmentId, f32, bool)]) {
    let toggle = SegmentClick::Action(Action::ToggleStatusLine);
    let segments: Vec<_> = segments
        .iter()
        .map(|(id, left_px, clickable)| (*id, *left_px, clickable.then(|| toggle.clone())))
        .collect();
    present_with(app, window_id, &segments);
}

/// The window's accessibility state.
fn access(app: &App, window_id: WindowId) -> &WindowAccessibility {
    app.accessibility.get(&window_id).expect("terminal windows have accessibility")
}

/// The last tree the window sent.
fn last_tree(app: &App, window_id: WindowId) -> TreeUpdate {
    access(app, window_id).sent.last().cloned().expect("a tree was sent")
}

/// The node with `id` in `tree`.
fn node(tree: &TreeUpdate, id: NodeId) -> Option<&Node> {
    tree.nodes.iter().find(|(node_id, _)| *node_id == id).map(|(_, node)| node)
}

/// Connect an assistive technology to `window_id` and run the dispatch's flush at `now`.
fn activate(app: &mut App, window_id: WindowId, now: Instant) {
    app.handle_accessibility_request(window_id, AccessibilityRequest::InitialTree);
    let _ = app.flush_accessibility(now);
}

/// The announcement node's current text and urgency.
fn announced(app: &App, window_id: WindowId) -> Option<Announcement> {
    access(app, window_id).announcement.clone()
}

/// A mouse press of `button` at the window's recorded pointer.
fn press(button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput { device_id: DeviceId::dummy(), state: ElementState::Pressed, button }
}

/// The body of `function` in `source`, up to the next function at the same indent.
fn body<'source>(source: &'source str, function: &str) -> &'source str {
    let start = source.find(function).unwrap_or_else(|| panic!("{function}"));
    let rest = &source[start + function.len()..];
    // The nearest next function start, whatever its visibility.
    let end = ["\n    fn ", "\n    pub(super) fn ", "\n    pub(in crate::app) fn "]
        .iter()
        .filter_map(|start| rest.find(start))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

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
            include_str!("misc.rs"),
            "pub(super) fn create_new_terminal_window(",
            Some("window.set_visible(true)"),
        ),
        (include_str!("tear_out.rs"), "fn create_warm_window(", None),
        (include_str!("tear_out.rs"), "fn prepare_tear_out_destination(", None),
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
    for source in [include_str!("window_event.rs"), include_str!("child_window_redraw.rs")] {
        assert!(!source.contains("flush_accessibility"));
        assert!(!source.contains("flush_window_accessibility"));
    }
    assert!(!body(include_str!("status_line.rs"), "pub(super) fn observe_status_pane(")
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
    let events = body(include_str!("window_event.rs"), "pub(super) fn do_window_event(");
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
    let keyboard =
        body(include_str!("window_keyboard.rs"), "pub(super) fn handle_window_keyboard(");
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
    let shared = body(include_str!("window_event.rs"), "pub(super) fn route_shared_pointer(");
    assert!(
        shared.find("self.end_status_focus_outside(win_id)").unwrap()
            < shared.find("self.field_pointer_event(").unwrap()
    );
    let events = include_str!("window_event.rs");
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

#[test]
fn enter_on_the_production_memory_segment_opens_memory_usage() {
    // Finding #19: Enter runs the production `mem` click through the real action routing.
    let (mut app, window_id) = app_with_window();
    let inputs = sonicterm_ui::status_line::StatusLineInputs {
        mem: sonicterm_ui::status_line::MemInput { bytes: Some(1 << 30), ..Default::default() },
        ..Default::default()
    };
    let view = app.accessible_view(&[StatusSegmentId::Mem], &inputs);
    let click = view
        .segments
        .iter()
        .find(|segment| segment.id == StatusSegmentId::Mem)
        .and_then(|segment| segment.click.clone());
    assert!(click.is_some(), "the production segment is clickable");
    present_with(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, click)]);
    app.focus_status_line(window_id);
    assert!(app.status_focus_key(window_id, &Key::Named(NamedKey::Enter), false));
    assert!(app.memory_report.open_requested, "Memory Usage was requested");
}

#[test]
fn an_assistive_technology_click_obeys_readonly_and_stays_on_its_window() {
    // Finding #19: an assistive technology's Click runs through the source window's action
    // routing, so READONLY there refuses a disallowed action, and an allowed one acts on that
    // window even while another is frontmost.
    let (mut app, window_id) = app_with_window();
    let main = app.main_window_id.unwrap();
    app.__test_set_frontmost_window(Some(main));
    assert!(!crate::app::keymap_dispatch::read_only_allows_action(&Action::NewTab));
    present_with(
        &mut app,
        window_id,
        &[
            (StatusSegmentId::Mem, 0.0, Some(SegmentClick::Action(Action::NewTab))),
            (StatusSegmentId::Cpu, 50.0, Some(SegmentClick::Action(Action::RenameWindow))),
        ],
    );
    activate(&mut app, window_id, Instant::now());
    app.windows.get_mut(&window_id).unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
    let tabs = app.windows[&window_id].tabs.len();
    let state = access(&app, window_id);
    let targets: HashMap<StatusSegmentId, Option<RequestTarget>> =
        [StatusSegmentId::Mem, StatusSegmentId::Cpu]
            .into_iter()
            .map(|id| (id, state.resolve(segment_node(id).0)))
            .collect();
    let click = |id: StatusSegmentId| AccessibilityRequest::Click {
        node: segment_node(id).0,
        target: targets[&id],
    };
    app.handle_accessibility_request(window_id, click(StatusSegmentId::Mem));
    assert_eq!(app.windows[&window_id].tabs.len(), tabs, "READONLY refused the new tab");
    app.windows.get_mut(&window_id).unwrap().copy_mode = None;
    app.handle_accessibility_request(window_id, click(StatusSegmentId::Cpu));
    assert_eq!(app.palette_attached_window, Some(window_id), "rename opened on the source");
}

#[test]
fn keyboard_focus_moves_clicks_and_returns_without_reaching_the_terminal() {
    // `focus_status_line` focuses the first clickable segment; arrows, Home and End move;
    // Escape and a printable key return focus; Enter clicks; nothing reaches a PTY.
    let (mut app, window_id) = app_with_window();
    app.__test_enable_pty_write_log();
    present(
        &mut app,
        window_id,
        &[
            (StatusSegmentId::Scroll, 0.0, false),
            (StatusSegmentId::Mem, 50.0, true),
            (StatusSegmentId::Cpu, 100.0, false),
        ],
    );
    let focus = |app: &App| access(app, window_id).focus;
    app.focus_status_line(window_id);
    assert_eq!(focus(&app), Some(StatusSegmentId::Mem), "the first clickable segment");
    for (key, expected) in [
        (NamedKey::ArrowRight, StatusSegmentId::Cpu),
        (NamedKey::ArrowRight, StatusSegmentId::Cpu),
        (NamedKey::Home, StatusSegmentId::Scroll),
        (NamedKey::ArrowLeft, StatusSegmentId::Scroll),
        (NamedKey::End, StatusSegmentId::Cpu),
        (NamedKey::ArrowLeft, StatusSegmentId::Mem),
    ] {
        assert!(app.status_focus_key(window_id, &Key::Named(key), false));
        assert_eq!(focus(&app), Some(expected), "{key:?}");
    }
    assert!(app.status_focus_key(window_id, &Key::Named(NamedKey::Escape), false));
    assert_eq!(focus(&app), None);
    assert!(
        !app.status_focus_key(window_id, &Key::Character(SmolStr::new("a")), false),
        "the terminal's again"
    );
    app.focus_status_line(window_id);
    assert!(
        app.status_focus_key(window_id, &Key::Character(SmolStr::new("a")), false),
        "consumed, not typed"
    );
    assert_eq!(focus(&app), None);
    app.focus_status_line(window_id);
    assert!(app.status_focus_key(window_id, &Key::Named(NamedKey::Enter), false));
    assert!(!app.status_line_shown(), "Enter clicked the memory segment");
    assert!(app.__test_drain_pty_writes().is_empty(), "no key reached a PTY");
}

#[test]
fn ime_blur_and_tab_changes_end_strip_focus() {
    // Preedit is suppressed and a commit is discarded while the strip has focus; losing window
    // focus or changing tab ends strip focus.
    let (mut app, window_id) = app_with_window();
    app.__test_enable_pty_write_log();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    let focus = |app: &App| access(app, window_id).focus;
    app.focus_status_line(window_id);
    app.handle_window_ime(window_id, Ime::Preedit("か".into(), None));
    assert_eq!(focus(&app), Some(StatusSegmentId::Mem), "preedit is suppressed");
    app.handle_window_ime(window_id, Ime::Commit("漢字".into()));
    assert_eq!(focus(&app), None, "a commit returns focus");
    assert!(app.__test_drain_pty_writes().is_empty(), "the commit wrote nothing");
    app.focus_status_line(window_id);
    app.handle_window_focus_changed(window_id, false);
    assert_eq!(focus(&app), None, "blur ends focus");
    app.focus_status_line(window_id);
    app.windows.get_mut(&window_id).unwrap().tabs.activate(0);
    assert!(!app.status_focus_key(window_id, &Key::Named(NamedKey::ArrowRight), false));
    assert_eq!(focus(&app), None, "a tab change ends focus");
}

#[test]
fn focus_never_activates_anything() {
    // An assistive technology's Focus moves focus only and shows in the next tree.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let node_id = segment_node(StatusSegmentId::Mem).0;
    let target = access(&app, window_id).resolve(node_id);
    app.handle_accessibility_request(
        window_id,
        AccessibilityRequest::Focus { node: node_id, target },
    );
    assert!(app.status_line_shown(), "Focus opened nothing");
    assert_eq!(access(&app, window_id).focus, Some(StatusSegmentId::Mem));
    let _ = app.flush_accessibility(Instant::now());
    assert_eq!(last_tree(&app, window_id).focus, segment_node(StatusSegmentId::Mem));
}

#[test]
fn the_segment_checklist_takes_keys_from_strip_focus() {
    // A right press in the strip opens the segment checklist; strip focus ends there, so the
    // checklist, not the strip, receives the next arrow key.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    app.focus_status_line(window_id);
    assert_eq!(access(&app, window_id).focus, Some(StatusSegmentId::Mem));
    assert!(app.right_press_status_line(window_id, 10.0, 510.0), "the press opened the checklist");
    assert_eq!(access(&app, window_id).focus, None, "the checklist ends strip focus");
    assert!(!app.status_focus_key(window_id, &Key::Named(NamedKey::ArrowRight), false));
}

#[test]
fn focus_ring_redraws_go_through_the_dispatch_record() {
    // Moving or ending strip focus asks for a frame only through the dispatch record, so the
    // strip pass, the checklist and the focus ring ask each window at most once per dispatch.
    let source = include_str!("accessibility.rs");
    let ring = body(source, "fn apply_focus_ring(");
    assert!(!ring.contains("request_window_redraw()"), "no direct ask");
    assert_eq!(ring.matches("self.request_redraw_once(window_id)").count(), 1);
    for function in ["pub(super) fn set_status_focus(", "pub(super) fn end_status_focus("] {
        let text = body(source, function);
        assert!(text.contains("self.apply_focus_ring(window_id)"), "{function}");
        assert!(!text.contains("request_window_redraw()"), "{function}");
    }
}

#[test]
fn health_announces_when_no_strip_list_names_it() {
    // Findings #5 and #13: a segment absent from every visual list still announces from its own
    // formatter over the current inputs.
    let (mut app, window_id) = app_with_window();
    app.config.status_line.left = Some(Vec::new());
    app.config.status_line.center = Some(Vec::new());
    app.config.status_line.right = Some(vec![StatusSegmentId::Mem]);
    let _ = app.sync_status_line(Instant::now());
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    app.set_config_reload_error(Some(&anyhow::anyhow!("bad key")));
    let _ = app.flush_accessibility(Instant::now());
    let said = announced(&app, window_id).expect("health announced though no list names it");
    assert!(said.text.starts_with("Error:") && said.assertive, "{}", said.text);
}

/// A headless App with the strip on and one torn-out terminal window holding `titles` as tabs.
fn app_with_tabs(titles: &[&str]) -> (App, WindowId) {
    let mut config = Config::default();
    config.status_line.enabled = true;
    let mut app = App::new(Theme::default(), config, Keymap::default());
    let window_id = app.__test_seed_child_window(titles);
    let _ = app.sync_status_line(Instant::now());
    (app, window_id)
}

#[test]
fn a_queued_click_whose_node_now_carries_another_action_is_dropped() {
    // Cycle 2 finding 1: a reload that keeps a node's bounds but gives it another action replaces
    // what a queued click named, so the click is dropped rather than running the new action.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let mem = segment_node(StatusSegmentId::Mem).0;
    let target = access(&app, window_id).resolve(mem);
    present_with(
        &mut app,
        window_id,
        &[(StatusSegmentId::Mem, 0.0, Some(SegmentClick::Action(Action::RenameWindow)))],
    );
    app.handle_accessibility_request(window_id, AccessibilityRequest::Click { node: mem, target });
    assert_eq!(app.palette_attached_window, None, "the new action did not run");
    assert!(app.status_line_shown(), "nor did the old one");
}

#[test]
fn a_queued_click_after_a_resize_before_a_new_layout_is_dropped() {
    // Cycle 2 finding 1: a resize replaces what a queued click named even before a frame commits
    // a new layout.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let mem = segment_node(StatusSegmentId::Mem).0;
    let target = access(&app, window_id).resolve(mem);
    let resized = WindowEvent::Resized(winit::dpi::PhysicalSize::new(640, 400));
    app.forward_accessibility_event(window_id, &resized);
    app.handle_accessibility_request(window_id, AccessibilityRequest::Click { node: mem, target });
    assert!(app.status_line_shown(), "the click against the pre-resize tree was dropped");
}

#[test]
fn a_request_resolved_against_the_old_tree_is_dropped_after_its_replacement() {
    // Round 4 item 1, QA's barrier: tree 1 toggles the strip; a reload moves the node and gives it
    // another action, so tree 2 is built and sent; a request resolved between building and sending
    // names tree 1's target and, delivered after tree 2 is in place, is dropped.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let mem = segment_node(StatusSegmentId::Mem).0;
    let tree_one = access(&app, window_id).resolve(mem);
    present_with(
        &mut app,
        window_id,
        &[(StatusSegmentId::Mem, 50.0, Some(SegmentClick::Action(Action::RenameWindow)))],
    );
    app.accessibility.get_mut(&window_id).unwrap().resolve_before_send = Some(mem);
    let _ = app.flush_accessibility(Instant::now());
    let target = access(&app, window_id).resolved_before_send;
    assert_eq!(target, tree_one, "resolved against the tree the adapter still held");
    assert_ne!(access(&app, window_id).resolve(mem), tree_one, "tree 2's target is published");
    app.handle_accessibility_request(window_id, AccessibilityRequest::Click { node: mem, target });
    assert_eq!(app.palette_attached_window, None, "neither tree 2's action ran");
    assert!(app.status_line_shown(), "nor tree 1's");
}

#[test]
fn a_reload_that_stops_announcing_a_segment_drops_its_pending_transition() {
    // Cycle 2 finding 4: a recovery waiting for its interval is never spoken once a reload takes
    // `health` off the `announce` list, and it arms no wake.
    let (mut app, window_id) = app_with_window();
    let start = Instant::now();
    activate(&mut app, window_id, start);
    app.set_config_reload_error(Some(&anyhow::anyhow!("bad key")));
    let _ = app.flush_accessibility(start + Duration::from_secs(1));
    app.set_config_reload_error(None);
    let wake = app.flush_accessibility(start + Duration::from_secs(2));
    assert!(wake.is_some(), "the recovery waits for its interval");
    app.config.status_line.accessibility.announce = Vec::new();
    let _ = app.sync_status_line(start + Duration::from_secs(3));
    let wake = app.flush_accessibility(start + Duration::from_secs(3));
    assert_eq!(wake, None, "no deadline for a segment that no longer announces");
    let _ = app.flush_accessibility(start + Duration::from_secs(6));
    assert!(announced(&app, window_id).unwrap().text.starts_with("Error:"), "nothing new spoken");
}

#[test]
fn a_pending_health_transition_follows_the_current_state() {
    // Cycle 2 finding 5: warn at 0, error at 1 (waiting), error cleared but still warning at 2,
    // flush at 5: the cleared error is never delivered; the current warning stands.
    let (mut app, window_id) = app_with_window();
    let start = Instant::now();
    activate(&mut app, window_id, start);
    app.status_line.render_override = Some(sonicterm_ui::status_line::RenderHealth::SoftwareForced);
    let _ = app.flush_accessibility(start);
    let warning = announced(&app, window_id).expect("the warning is announced");
    assert!(warning.text.starts_with("Warning:"), "{}", warning.text);
    app.set_config_reload_error(Some(&anyhow::anyhow!("bad key")));
    let _ = app.flush_accessibility(start + Duration::from_secs(1));
    app.set_config_reload_error(None);
    let _ = app.flush_accessibility(start + Duration::from_secs(2));
    let _ = app.flush_accessibility(start + Duration::from_secs(5));
    let current = announced(&app, window_id).unwrap();
    assert!(current.text.starts_with("Warning:"), "never the cleared error: {}", current.text);
}

#[test]
fn a_segment_click_hands_the_keyboard_to_what_it_opens() {
    // Cycle 2 finding 6: strip focus ends before a click runs, so the rename editor it opens owns
    // keys and IME at once, and the strip no longer acts on arrows or Enter.
    let (mut app, window_id) = app_with_window();
    present_with(
        &mut app,
        window_id,
        &[(StatusSegmentId::Mem, 0.0, Some(SegmentClick::Action(Action::RenameWindow)))],
    );
    app.focus_status_line(window_id);
    assert!(app.status_focus_key(window_id, &Key::Named(NamedKey::Enter), false));
    assert_eq!(app.palette_attached_window, Some(window_id), "the rename editor opened");
    assert_eq!(access(&app, window_id).focus, None, "strip focus ended before the click ran");
    let before = app.command_palette.query().to_owned();
    app.handle_window_ime(window_id, Ime::Commit("x".into()));
    assert_eq!(
        app.command_palette.query(),
        format!("{before}x"),
        "the character reached the editor"
    );
    assert!(!app.status_focus_key(window_id, &Key::Named(NamedKey::ArrowRight), false));
    assert!(!app.status_focus_key(window_id, &Key::Named(NamedKey::Enter), false));
}

#[test]
fn the_charge_always_equals_the_capacity_estimate_and_disconnect_frees_the_model() {
    // Cycle 2 finding 7: the ledger charge is the capacity-based estimate while connected and
    // after many updates; a disconnect frees the model, so both are zero; tab marks are bounded
    // by the segment count, not the tab count.
    let titles: Vec<String> = (0..30).map(|index| format!("tab {index}")).collect();
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    let (mut app, window_id) = app_with_tabs(&titles);
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    let owner = app.windows[&window_id].owner.as_ref().map(|guard| guard.id()).expect("owner");
    let charged = |app: &App| {
        app.__test_owner_snapshot(owner).unwrap().owner_class_bytes
            [ResourceClass::AccessibilityTree]
    };
    let start = Instant::now();
    activate(&mut app, window_id, start);
    assert_eq!(charged(&app), access(&app, window_id).retained_bytes());
    for tab in 0..29 {
        app.windows.get_mut(&window_id).unwrap().tabs.mark(tab, |marks| marks.bell = true);
    }
    for step in 1..100_u64 {
        let error = (step % 2 == 0).then(|| anyhow::anyhow!("bad key {step}"));
        app.set_config_reload_error(error.as_ref());
        let _ = app.flush_accessibility(start + Duration::from_millis(step * 700));
        assert_eq!(charged(&app), access(&app, window_id).retained_bytes(), "step {step}");
    }
    app.handle_accessibility_request(window_id, AccessibilityRequest::Deactivated);
    assert_eq!(charged(&app), 0);
    assert_eq!(access(&app, window_id).retained_bytes(), 0, "the model itself is freed");
}

#[test]
fn spoken_values_update_while_frame_collection_is_contended() {
    // Cycle 2 finding 8: the real frame collection finds a visible parser held and reports
    // `Contended`; no layout is committed, yet a sampler tick still reaches the tree.
    let (mut app, window_id) = app_with_window();
    present(&mut app, window_id, &[(StatusSegmentId::Mem, 0.0, true)]);
    activate(&mut app, window_id, Instant::now());
    let rect = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 480.0);
    let parsers: Vec<_> =
        app.windows[&window_id].panes.values().map(|pane| Arc::clone(&pane.parser)).collect();
    let held: Vec<_> = parsers.iter().map(|parser| parser.lock()).collect();
    let sources = app.child_visible_frame_sources(window_id, rect).expect("a live owner");
    let collected = sources.try_collect(|| ());
    assert!(
        matches!(collected, Err(crate::app::visible_frame::FrameUnavailable::Contended { .. })),
        "the held parser makes the real collection contended"
    );
    drop(collected);
    app.observe_status_pane(window_id, Default::default());
    let _ = app.sync_status_line(Instant::now());
    let _ = app.flush_accessibility(Instant::now());
    drop(held);
    let name = node(&last_tree(&app, window_id), segment_node(StatusSegmentId::Mem))
        .and_then(|node| node.label().map(str::to_owned))
        .unwrap_or_default();
    assert!(name.chars().any(|character| character.is_ascii_digit()), "{name}");
}

#[test]
fn a_mouse_click_on_a_segment_hands_the_keyboard_to_what_it_opens() {
    // Round 4 item 2: a mouse click goes through the same handoff as Enter and an assistive
    // technology's Click, so the rename editor it opens receives the next character.
    let (mut app, window_id) = app_with_window();
    present_with(
        &mut app,
        window_id,
        &[(StatusSegmentId::Mem, 0.0, Some(SegmentClick::Action(Action::RenameWindow)))],
    );
    app.focus_status_line(window_id);
    assert!(app.press_status_line(window_id, 10.0, 510.0), "the press was on the strip");
    assert_eq!(app.palette_attached_window, Some(window_id), "the rename editor opened");
    assert_eq!(access(&app, window_id).focus, None);
    let before = app.command_palette.query().to_owned();
    app.handle_window_ime(window_id, Ime::Commit("x".into()));
    assert_eq!(app.command_palette.query(), format!("{before}x"));
}

/// Seed `count` tabs, connect a reader, mark `marked` tabs with `marks` before the connection,
/// then ring tab `rings`: returns what the reader heard.
fn ring_after_marks(count: usize, marked: usize, marks: fn(&mut TabMarks), rings: usize) -> String {
    let titles: Vec<String> = (0..count).map(|index| format!("tab {index}")).collect();
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    let (mut app, window_id) = app_with_tabs(&titles);
    for tab in 0..marked {
        app.windows.get_mut(&window_id).unwrap().tabs.mark(tab, marks);
    }
    let start = Instant::now();
    activate(&mut app, window_id, start);
    app.windows.get_mut(&window_id).unwrap().tabs.mark(rings, |marks| marks.bell = true);
    let _ = app.flush_accessibility(start + Duration::from_secs(1));
    announced(&app, window_id).map(|said| said.text).unwrap_or_default()
}

#[test]
fn a_new_mark_is_announced_however_many_tabs_are_already_marked() {
    // Round 4 item 3: a mark is detected when its tab gets it, never by comparing remembered
    // marks, so a 22nd ringing tab and an 8th after 7 fully marked tabs are both announced.
    let heard = ring_after_marks(30, 21, |marks| marks.bell = true, 21);
    assert!(heard.contains("22") && heard.contains("bell"), "{heard}");
    let heard = ring_after_marks(
        30,
        7,
        |marks| {
            marks.bell = true;
            marks.notify = true;
            marks.finished_secs = Some(3);
        },
        7,
    );
    assert!(heard.contains('8') && heard.contains("bell"), "{heard}");
}

#[test]
fn the_retained_model_does_not_grow_with_the_tab_count() {
    // Round 4 item 3: however many tabs carry marks, the retained model is the same size. Both
    // counts name a three-digit tab, so the spoken text is the same length.
    let retained = |count: usize| {
        let titles: Vec<String> = (0..count).map(|index| format!("tab {index}")).collect();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        let (mut app, window_id) = app_with_tabs(&titles);
        let start = Instant::now();
        activate(&mut app, window_id, start);
        for tab in 0..count - 1 {
            app.windows.get_mut(&window_id).unwrap().tabs.mark(tab, |marks| marks.bell = true);
        }
        let _ = app.flush_accessibility(start + Duration::from_secs(1));
        access(&app, window_id).retained_bytes()
    };
    assert_eq!(retained(200), retained(900));
}

/// The model's bytes as allocated storage: hash tables through the repository's shared table
/// estimate, vectors and strings by capacity.
fn allocated_bytes(state: &WindowAccessibility) -> usize {
    sonicterm_types::retained_hash_table_bytes::<StatusSegmentId, (Instant, String)>(
        state.announced.capacity(),
    ) + state.announced.values().map(|(_, said)| said.capacity()).sum::<usize>()
        + state.queue.capacity() * std::mem::size_of::<Pending>()
        + state.queue.iter().map(|pending| pending.text.capacity()).sum::<usize>()
        + state.structure.segments.capacity() * std::mem::size_of::<Exposed>()
        + state.announcement.as_ref().map_or(0, |said| said.text.capacity())
        + state.health_text.as_ref().map_or(0, String::capacity)
        + sonicterm_types::retained_hash_table_bytes::<u64, RequestTarget>(
            state.targets.lock().unwrap().capacity(),
        )
}

#[test]
fn retained_bytes_count_allocated_storage() {
    // Round 4 item 4: the charged figure is allocated storage, the hash table's buckets and
    // control bytes included, not the payload it holds.
    let (mut app, window_id) = app_with_window();
    present(
        &mut app,
        window_id,
        &[(StatusSegmentId::Mem, 0.0, true), (StatusSegmentId::Cpu, 50.0, false)],
    );
    let start = Instant::now();
    activate(&mut app, window_id, start);
    for step in 1..60_u64 {
        let error = (step % 2 == 0).then(|| anyhow::anyhow!("bad key {step}"));
        app.set_config_reload_error(error.as_ref());
        let _ = app.flush_accessibility(start + Duration::from_millis(step * 700));
    }
    let state = access(&app, window_id);
    assert!(state.announced.capacity() > 0, "the table holds something to count");
    assert_eq!(state.retained_bytes(), allocated_bytes(state));
}

#[test]
fn a_finished_announcement_keeps_its_duration_after_the_tab_is_visited() {
    // A 134-second command finishes on an inactive tab, and the tab is visited, which clears its
    // marks, before the flush: the announcement still says how long the command ran.
    let (mut app, window_id) = app_with_tabs(&["first", "second"]);
    let start = Instant::now();
    activate(&mut app, window_id, start);
    app.windows.get_mut(&window_id).unwrap().tabs.mark(0, |marks| marks.finished_secs = Some(134));
    app.windows.get_mut(&window_id).unwrap().tabs.activate(0);
    let _ = app.flush_accessibility(start + Duration::from_secs(1));
    let heard = announced(&app, window_id).map(|said| said.text).unwrap_or_default();
    assert!(heard.contains(&format_duration(134)), "{heard}");
}
