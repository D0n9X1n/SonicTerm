//! Accessibility target validation and keyboard focus contracts.

use super::super::accessibility_tests::*;
use super::*;

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
    let source = concat!(
        include_str!("accessibility.rs"),
        "\n",
        include_str!("accessibility_actions.rs"),
        "\n",
        include_str!("accessibility_tree.rs")
    );
    let ring = body(source, "fn apply_focus_ring(");
    assert!(!ring.contains("request_window_redraw()"), "no direct ask");
    assert_eq!(ring.matches("self.request_redraw_once(window_id)").count(), 1);
    for function in ["fn set_status_focus(", "fn end_status_focus("] {
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
