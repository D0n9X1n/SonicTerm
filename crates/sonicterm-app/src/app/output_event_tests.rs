use std::sync::atomic::Ordering;

use super::*;
use crate::app::redraw::{FrameSettlement, RedrawCause};

/// A main window plus a visible child window whose tab 0 is active and tab 1 is in the background.
fn owners() -> (App, WindowId, WindowId) {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child", "background"]);
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    (app, main, child)
}

/// Native redraw asks made so far on this test thread.
fn native_requests() -> u64 {
    crate::app::window_state::window_redraw_requests()
}

/// The pane shown by tab `tab_index` of window `id`.
fn tab_pane(app: &App, id: WindowId, tab_index: usize) -> u64 {
    app.windows[&id].tab_states[tab_index].active_pane
}

/// Parse `bytes` as the pane's VT worker would: publish the batch and bump its output generation.
fn worker_batch(app: &App, id: WindowId, pane_id: u64, bytes: &[u8]) -> Vec<Vec<u8>> {
    let handles =
        crate::app::spawn_pane::PaneVtHandles::from_pane_state(&app.windows[&id].panes[&pane_id]);
    let mut replies = Vec::new();
    crate::app::spawn_pane::process_pane_vt_batch_and_publish(
        &handles,
        bytes,
        &mut None,
        None,
        |reply| replies.push(reply),
    );
    replies
}

/// Flush the pane's output as its worker does; returns the windows it queued an event for.
fn worker_flush(app: &App, id: WindowId, pane_id: u64) -> Vec<WindowId> {
    let pane = &app.windows[&id].panes[&pane_id];
    let mut queued = Vec::new();
    crate::app::spawn_pane::send_output_redraw(
        &pane.redraw_target,
        &pane.output_outstanding,
        None,
        |window| {
            queued.push(window);
            true
        },
    );
    queued
}

/// Settle one frame of `id` as a presented redraw would, acknowledging its pane generations.
fn present(app: &mut App, id: WindowId) {
    let snapshot = app.snapshot_window_redraw(id).unwrap();
    app.finish_window_redraw(id, &snapshot, FrameSettlement::Presented, Instant::now());
}

/// A title change, a bell and a reply-only batch in the active tab each advance the visible
/// generation, so servicing the pane's output event requests one frame for each.
#[test]
fn active_tab_title_bell_and_reply_only_output_each_request_a_frame() {
    for (name, bytes) in
        [("title", &b"\x1b]2;fresh title\x07"[..]), ("bell", b"\x07"), ("reply only", b"\x1b[c")]
    {
        let (mut app, _, child) = owners();
        let pane = tab_pane(&app, child, 0);
        *app.windows[&child].panes[&pane].redraw_target.lock() = Some(child);
        let replies = worker_batch(&app, child, pane, bytes);
        if name == "reply only" {
            assert!(!replies.is_empty(), "the DA1 query produces a reply");
        }
        assert_eq!(worker_flush(&app, child, pane), [child], "{name}");
        let before = native_requests();
        app.service_output_event(
            OutputEvent::Pane { window_id: child, pane_id: pane },
            Instant::now(),
        );
        assert_eq!(native_requests() - before, 1, "{name}");
        assert!(app.windows[&child].redraw.request_in_flight, "{name}");
    }
}

/// Output that lands in a background tab is kept in the grid and its OSC title in the parser;
/// switching to that tab marks a topology cause, and the frame the child redraw assembles shows
/// both. A headless app has no renderer, where the child redraw returns before it collects, so
/// this follows the same production steps up to glyph emission: visible-frame sources, collection,
/// viewport reconciliation, the title refresh on the window's own tab bar, and `PaneRender`s.
#[test]
fn switching_to_a_background_tab_shows_its_latest_output_and_title() {
    use std::collections::{BTreeSet, HashMap};
    let (mut app, _, child) = owners();
    let pane = tab_pane(&app, child, 1);
    *app.windows[&child].panes[&pane].redraw_target.lock() = Some(child);
    worker_batch(&app, child, pane, b"\x1b]2;background title\x07latest line");
    worker_flush(&app, child, pane);
    app.service_output_event(OutputEvent::Pane { window_id: child, pane_id: pane }, Instant::now());
    let topology = app.windows[&child].redraw.cause_generation(RedrawCause::Topology);

    assert!(app.__test_invoke_activate_tab_in_child(child, 1));
    assert!(app.windows[&child].redraw.cause_generation(RedrawCause::Topology) > topology);

    let rect = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 480.0);
    let sources = app.child_visible_frame_sources(child, rect).ok().expect("valid owner");
    assert_eq!((sources.tab_index, sources.active_id()), (1, pane));
    let crate::app::visible_frame::HeldVisibleFrame { snapshot, mut guards, mut images } =
        sources.try_collect(|| app.snapshot_window_redraw(child)).ok().expect("uncontended");
    let window = app.windows.get_mut(&child).unwrap();
    let viewports = sources.reconcile_viewports(&mut window.panes, &guards).ok().expect("owner");
    crate::app::refresh_active_tab_title(
        &mut window.tabs,
        &window.panes[&pane],
        &guards[sources.active_pos].1,
        sources.tab_index,
    );
    assert!(window.tabs.active().unwrap().title.contains("background title"));
    let renders = crate::app::visible_frame::pane_renders(
        &mut guards,
        &mut images,
        &viewports,
        pane,
        &BTreeSet::new(),
        &HashMap::new(),
    );
    let render = renders.iter().find(|render| render.id == pane).expect("switched-to pane");
    assert!(render.is_active);
    assert!(render.grid.dirty_count() > 0, "the frame redraws the pane from its grid");
    let row: String = render.grid.row_at_abs(0).unwrap().iter().map(|cell| cell.ch).collect();
    assert!(row.starts_with("latest line"), "{row:?}");
    drop(renders);
    drop(guards);
    app.finish_window_redraw(
        child,
        &snapshot.expect("snapshot"),
        FrameSettlement::Presented,
        Instant::now(),
    );
    let state = &app.windows[&child].panes[&pane];
    assert_eq!(state.observed_output_generation, state.output_generation.load(Ordering::Acquire));
}

/// The worker's token swap may come before or after the event loop's acknowledgement; either way
/// the queued or a fresh event is serviced after the generation was bumped, and the latest
/// generation is shown.
#[test]
fn both_orders_of_worker_swap_and_acknowledgement_show_the_latest_output() {
    for swap_before_acknowledgement in [true, false] {
        let (mut app, _, child) = owners();
        let pane = tab_pane(&app, child, 0);
        *app.windows[&child].panes[&pane].redraw_target.lock() = Some(child);
        worker_batch(&app, child, pane, b"first");
        assert_eq!(worker_flush(&app, child, pane), [child]);
        let event = OutputEvent::Pane { window_id: child, pane_id: pane };
        if swap_before_acknowledgement {
            worker_batch(&app, child, pane, b"second");
            assert!(worker_flush(&app, child, pane).is_empty(), "coalesced into the queued event");
            app.service_output_event(event, Instant::now());
        } else {
            app.service_output_event(event, Instant::now());
            present(&mut app, child);
            worker_batch(&app, child, pane, b"second");
            assert_eq!(worker_flush(&app, child, pane), [child], "the acknowledged token sends");
            app.service_output_event(event, Instant::now());
        }
        assert!(app.windows[&child].redraw.request_in_flight, "the latest output requests a frame");
        present(&mut app, child);
        let window = &app.windows[&child];
        let latest = window.panes[&pane].output_generation.load(Ordering::Acquire);
        assert_eq!(latest, 2);
        assert_eq!(window.panes[&pane].observed_output_generation, latest);
        assert!(!window.panes[&pane].output_outstanding.load(Ordering::Acquire));
    }
}

/// A worker flush that lands at the pause point between the acknowledgement and the visible-output
/// check finds the token clear and sends a fresh event, and servicing it shows the latest output.
/// Were the acknowledgement after the check, that flush would coalesce into the serviced event.
#[test]
fn a_flush_between_acknowledgement_and_check_sends_a_fresh_event() {
    use std::{cell::RefCell, rc::Rc};
    let (mut app, _, child) = owners();
    let pane = tab_pane(&app, child, 0);
    *app.windows[&child].panes[&pane].redraw_target.lock() = Some(child);
    worker_batch(&app, child, pane, b"first");
    assert_eq!(worker_flush(&app, child, pane), [child]);
    let state = &app.windows[&child].panes[&pane];
    let handles = crate::app::spawn_pane::PaneVtHandles::from_pane_state(state);
    let (target, outstanding) = (state.redraw_target.clone(), state.output_outstanding.clone());
    let queued = Rc::new(RefCell::new(Vec::new()));
    let hook_queued = Rc::clone(&queued);
    super::pause_after_acknowledge(move || {
        crate::app::spawn_pane::process_pane_vt_batch_and_publish(
            &handles,
            b"second",
            &mut None,
            None,
            |_| {},
        );
        crate::app::spawn_pane::send_output_redraw(&target, &outstanding, None, |window| {
            hook_queued.borrow_mut().push(window);
            true
        });
    });
    let event = OutputEvent::Pane { window_id: child, pane_id: pane };

    app.service_output_event(event, Instant::now());

    assert_eq!(*queued.borrow(), [child], "the flush after the acknowledgement sends");
    assert!(app.windows[&child].redraw.request_in_flight);
    present(&mut app, child);
    app.service_output_event(event, Instant::now());
    present(&mut app, child);
    let state = &app.windows[&child].panes[&pane];
    assert_eq!(state.output_generation.load(Ordering::Acquire), 2);
    assert_eq!(state.observed_output_generation, 2, "the latest output is shown");
    assert!(!state.output_outstanding.load(Ordering::Acquire));
}

/// An output event queued before its pane's tab moves to another window is acknowledged on the
/// pane and requests a frame from the window that owns the pane now, never from the old owner.
#[test]
fn output_event_queued_before_a_transfer_requests_only_the_new_owner() {
    let (mut app, main, child) = owners();
    let pane = tab_pane(&app, main, 0);
    *app.windows[&main].panes[&pane].redraw_target.lock() = Some(main);
    worker_batch(&app, main, pane, b"moved");
    assert_eq!(worker_flush(&app, main, pane), [main]);
    // A headless destination needs explicit geometry to accept a tab.
    app.windows.get_mut(&child).unwrap().test_pane_viewport =
        Some((sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 500.0), 10.0, 20.0));
    app.transfer_tab(None, 0, Some(child), 2).unwrap();
    let main_causes = app.windows[&main].redraw.snapshot();
    let main_output = app.windows[&main].redraw.cause_generation(RedrawCause::Output);
    let child_output = app.windows[&child].redraw.cause_generation(RedrawCause::Output);
    app.windows.get_mut(&child).unwrap().redraw.request_in_flight = false;

    app.service_output_event(OutputEvent::Pane { window_id: main, pane_id: pane }, Instant::now());

    assert!(!app.windows[&child].panes[&pane].output_outstanding.load(Ordering::Acquire));
    assert_eq!(app.windows[&main].redraw.snapshot(), main_causes, "the old owner is untouched");
    assert_eq!(app.windows[&main].redraw.cause_generation(RedrawCause::Output), main_output);
    assert!(app.windows[&child].redraw.cause_generation(RedrawCause::Output) > child_output);
    assert!(app.windows[&child].redraw.request_in_flight);
}

/// A late output event for a pane retired after its worker cloned the redraw target finds no pane
/// and does nothing: no frame request and no cause in any window.
#[test]
fn late_output_event_for_a_retired_pane_does_nothing() {
    let (mut app, main, child) = owners();
    let pane = tab_pane(&app, child, 0);
    *app.windows[&child].panes[&pane].redraw_target.lock() = Some(child);
    worker_batch(&app, child, pane, b"last words");
    let cloned_target = *app.windows[&child].panes[&pane].redraw_target.lock();
    let retired = app.windows.get_mut(&child).unwrap().panes.remove(&pane).unwrap();
    app.retire_pane(retired);
    let causes = [main, child].map(|id| app.windows[&id].redraw.snapshot());
    let before = native_requests();

    app.service_output_event(
        OutputEvent::Pane { window_id: cloned_target.unwrap(), pane_id: pane },
        Instant::now(),
    );

    assert_eq!(native_requests(), before);
    assert_eq!([main, child].map(|id| app.windows[&id].redraw.snapshot()), causes);
    assert!(!app.windows[&child].redraw.request_in_flight);
}

/// Whether the variant use at `offset` in `code` is a match pattern: after its fields, `=>` or
/// `|` follows.
fn is_pattern(code: &str, offset: usize) -> bool {
    let rest = &code[offset..];
    let after_name = rest
        .find(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == ':')
        })
        .unwrap_or(rest.len());
    let mut tail = rest[after_name..].trim_start();
    if let Some(open) =
        tail.chars().next().filter(|character| *character == '{' || *character == '(')
    {
        let close = if open == '{' { '}' } else { ')' };
        let mut depth = 0usize;
        for (index, character) in tail.char_indices() {
            if character == open {
                depth += 1;
            } else if character == close {
                depth -= 1;
                if depth == 0 {
                    tail = tail[index + 1..].trim_start();
                    break;
                }
            }
        }
    }
    tail.starts_with("=>") || tail.starts_with('|')
}

/// Each output-event variant `text` builds (not matches), once per construction, in code only.
fn output_event_builders(text: &str) -> Vec<&'static str> {
    let (_, code) = crate::app::source_scan_support::code_views(text);
    let mut builders = Vec::new();
    for variant in ["UserEvent::PaneOutput", "UserEvent::RequestRedraw"] {
        for (offset, _) in code.match_indices(variant) {
            if !is_pattern(&code, offset) {
                builders.push(variant);
            }
        }
    }
    builders
}

/// Sources whose real code builds one `RequestRedraw` after a literal or comment that a naive
/// scan misreads, and sources whose only mention is inside a comment or literal.
const BUILDER_FIXTURES: [(&str, &str, usize); 7] = [
    (
        "escaped quote char literal",
        "fn case() {\n    let quote = '\\\"';\n    send(UserEvent::RequestRedraw(window));\n}\n",
        1,
    ),
    (
        "raw string with a quote inside",
        "fn case() {\n    let text = r#\"say \"hi\"\"#;\n    send(UserEvent::RequestRedraw(window));\n}\n",
        1,
    ),
    (
        "nested block comment",
        "fn case() {\n    /* outer /* inner */ UserEvent::RequestRedraw(window) */\n}\n",
        0,
    ),
    (
        "send inside a raw string",
        "fn case() {\n    let text = r#\"UserEvent::RequestRedraw(window)\"#;\n}\n",
        0,
    ),
    (
        "send inside a line comment",
        "fn case() {\n    // send(UserEvent::RequestRedraw(window));\n}\n",
        0,
    ),
    (
        "CRLF line endings",
        "fn case() {\r\n    // a comment\r\n    send(UserEvent::RequestRedraw(window));\r\n}\r\n",
        1,
    ),
    (
        "match pattern only",
        "fn case(event: UserEvent) {\n    match event {\n        UserEvent::PaneOutput { pane_id, .. } => drop(pane_id),\n        _ => {}\n    }\n}\n",
        0,
    ),
];

/// The builder scan finds every real construction and none hidden in comments or literals,
/// whatever literal or comment precedes it and whatever the line endings.
#[test]
fn builder_scan_sees_only_real_constructions() {
    let mut wrong = Vec::new();
    for (name, source, expected) in BUILDER_FIXTURES {
        let found = output_event_builders(source).len();
        if found != expected {
            wrong.push(format!("{name}: found {found}, expected {expected}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Both output arms of `do_user_event` call only `service_output_event`; in the app sources a
/// `PaneOutput` is built only by the VT worker in `spawn_pane.rs` and no `RequestRedraw` is built.
#[test]
fn output_arms_route_only_through_the_service_and_only_the_worker_builds_pane_output() {
    let (_, event_loop) =
        crate::app::source_scan_support::code_views(include_str!("event_loop.rs"));
    for variant in ["UserEvent::RequestRedraw", "UserEvent::PaneOutput"] {
        let start = event_loop.find(variant).unwrap_or_else(|| panic!("{variant} arm"));
        let arrow = start + event_loop[start..].find("=>").unwrap() + 2;
        let body = event_loop[arrow..].trim_start();
        let end = if body.starts_with('{') {
            let mut depth = 0usize;
            body.char_indices()
                .find(|(_, character)| {
                    match character {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                })
                .map_or(body.len(), |(index, _)| index + 1)
        } else {
            body.find(',').unwrap_or(body.len())
        };
        let body = &body[..end];
        assert_eq!(body.matches("self.").count(), 1, "{variant} arm: {body}");
        assert!(body.contains("self.service_output_event("), "{variant} arm: {body}");
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![root.clone()];
    let mut builders = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
                continue;
            }
            for variant in output_event_builders(&std::fs::read_to_string(&path).unwrap()) {
                builders.push(format!("{name}: {variant}"));
            }
        }
    }
    builders.sort();
    assert_eq!(builders, ["spawn_pane.rs: UserEvent::PaneOutput"]);
}
