//! Pins the tools that surface an agent's work: `focus_pane`, `set_title` and `notify` refuse
//! outside the control scope without changing anything, focus selects a hidden tab in the main or
//! a child window, a tab zoomed on another pane is refused, the window is raised only while
//! SonicTerm is active, titles and messages lose control and format characters, and one session's
//! messages are rate-limited and bounded by the notice queue.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use sonicterm_cfg::config::{Config, McpConfig, McpControl, McpCreateAnchor, McpIsolation};
use sonicterm_cfg::keymap::{Direction, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_grid::grid::Grid;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_vt::vt::{CaptureStagingPool, Parser};

use super::*;
use crate::app::mcp::agents::{AgentSessionId, LiveSessions, PaneOwner};
use crate::app::mcp::{
    tools_control, tools_read, CallScope, Caller, McpServer, McpShared, MAX_PENDING_NOTICES,
};
use crate::app::media::InlineMediaPool;
use crate::app::{next_pane_id, PaneState};

/// A headless app with private pools.
fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(InlineMediaPool::new())
}

/// What `session` may touch at `control` and `isolation`, with any create anchor.
fn access(control: McpControl, isolation: McpIsolation, session: AgentSessionId) -> Access {
    Access { control, isolation, create_anchor: McpCreateAnchor::Any, session }
}

/// An app with a user tab and, active after it, a tab whose pane `session` created; returns the
/// app, the user's pane and the agent's pane.
fn seeded(session: AgentSessionId) -> (App, u64, u64) {
    let mut app = app();
    let user = app.__test_seed_tab("user");
    let agent = app.__test_seed_tab("agent");
    app.main_mut().unwrap().panes.get_mut(&agent).unwrap().creator = PaneOwner::Agent(session);
    (app, user, agent)
}

/// Split `anchor`'s main tab with a PTY-less pane `owner` created; the anchor stays active.
fn split_headless(app: &mut App, anchor: u64, owner: PaneOwner) -> u64 {
    let pane_id = next_pane_id();
    let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
        Grid::new(80, 24),
        None,
        Arc::clone(&app.capture_staging_pool),
    )));
    let media_pool = Arc::clone(&app.inline_media_pool);
    let main = app.main_mut().unwrap();
    let mut pane = PaneState::new_with_media_pool(parser, None, &media_pool);
    pane.creator = owner;
    main.panes.attach_new(pane_id, pane);
    let tab = main.tab_states.iter_mut().find(|tab| tab.tree.contains_pane(anchor)).unwrap();
    assert!(tab.tree.split(anchor, Direction::Right, pane_id));
    pane_id
}

/// A refusal's code, or `None` when the call did its work.
fn code(reply: &LoopReply) -> Option<&'static str> {
    match reply {
        LoopReply::Refused(code, _) => Some(code),
        _ => None,
    }
}

/// The raise requests this thread recorded since the last call, which clears them.
fn take_raises() -> Vec<RaiseRequest> {
    RAISE_REQUESTS.with(|requests| std::mem::take(&mut *requests.borrow_mut()))
}

/// Install a running server at `control` that binds nothing.
fn install(app: &mut App, control: McpControl) -> Arc<McpShared> {
    let shared = McpShared::new(control, None);
    let config = McpConfig { enabled: true, control, ..McpConfig::default() };
    app.config.mcp = config;
    let owner = app.register_mcp_owner();
    app.services.mcp_server = Some(McpServer {
        shared: Arc::clone(&shared),
        endpoint: "test-endpoint".into(),
        _endpoint_guard: None,
        config,
        charges: HashMap::new(),
        owner,
        read_notice_due: false,
    });
    shared
}

/// Run tool `name` as a connection's worker does for a client named `client`, while this thread
/// serves the loop; the call's result.
fn call_through_loop(
    app: &mut App,
    shared: &Arc<McpShared>,
    name: &str,
    arguments: Value,
    client: &str,
) -> Value {
    let worker_shared = Arc::clone(shared);
    let params = json!({ "name": name, "arguments": arguments });
    let caller = Caller { self_pane: None, client: Some(client.to_owned()), initialized: true };
    let worker = std::thread::spawn(move || {
        let scope = CallScope::admit(&worker_shared, &Arc::new(AtomicBool::new(false)));
        tools_read::call(&worker_shared, &caller, &params, &scope)
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "{name} did not finish");
        app.drain_mcp_requests();
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.join().unwrap()
}

/// Off, a user pane under `"agent"`, another session's pane under `isolation = "session"` and
/// READONLY each refuse all three tools with their code; no tab is selected, retitled or
/// notified, and a refusal spends no message slot.
#[test]
fn every_surfacing_tool_refuses_outside_its_scope_and_changes_nothing() {
    let sessions = LiveSessions::default();
    let mine = sessions.open();
    let (mut app, user, agent) = seeded(mine);
    app.main_mut().unwrap().tabs.activate(0);
    let cases = [
        (access(McpControl::Off, McpIsolation::Shared, mine), agent, false, "control_off"),
        (access(McpControl::Agent, McpIsolation::Shared, mine), user, false, "out_of_scope"),
        (
            access(McpControl::Agent, McpIsolation::Session, sessions.open()),
            agent,
            false,
            "out_of_scope",
        ),
        (access(McpControl::Any, McpIsolation::Shared, mine), agent, true, "readonly"),
    ];
    for (who, pane_id, readonly, expected) in cases {
        app.main_mut().unwrap().copy_mode = readonly.then(|| CopyModeState::read_only_at((0, 0)));
        let label = format!("{who:?} pane {pane_id}");
        let focused = app.mcp_focus_pane(&sessions, who, pane_id);
        assert_eq!(code(&focused), Some(expected), "{label} focus");
        assert_eq!(code(&app.mcp_set_title(who, pane_id, "x")), Some(expected), "{label} title");
        let notified = app.mcp_notify(&sessions, who, pane_id, "x");
        assert_eq!(code(&notified), Some(expected), "{label} notify");
        let main = app.main().unwrap();
        assert_eq!(main.tabs.active_index(), 0, "{label} selected a tab");
        assert!(main.tabs.tabs().iter().all(|tab| tab.custom_title.is_none()), "{label} titled");
        assert!(main.notification.is_none(), "{label} notified");
    }
    app.main_mut().unwrap().copy_mode = None;
    let own = access(McpControl::Agent, McpIsolation::Session, mine);
    assert!(matches!(app.mcp_focus_pane(&sessions, own, agent), LoopReply::Focused { .. }));
}

/// Focus on a pane in a background main tab selects that tab and the pane within it; with
/// SonicTerm not active it asks only for the user's attention and reports no raise.
#[test]
fn focus_selects_a_hidden_main_tab_and_its_pane() {
    let sessions = LiveSessions::default();
    let mine = sessions.open();
    let (mut app, _user, agent) = seeded(mine);
    let helper = split_headless(&mut app, agent, PaneOwner::Agent(mine));
    app.main_mut().unwrap().tabs.activate(0);
    take_raises();
    let own = access(McpControl::Agent, McpIsolation::Shared, mine);
    let reply = app.mcp_focus_pane(&sessions, own, helper);
    assert!(matches!(reply, LoopReply::Focused { raised: false }));
    let main = app.main().unwrap();
    assert_eq!(main.tabs.active_index(), 1, "the helper's tab is selected");
    assert_eq!(main.tab_states[1].active_pane, helper, "the helper is the tab's pane");
    assert_eq!(take_raises(), [RaiseRequest::Attention]);
}

/// Focus on a pane in a child window's background tab selects that tab in the child.
#[test]
fn focus_selects_a_tab_in_a_child_window() {
    let sessions = LiveSessions::default();
    let mine = sessions.open();
    let mut app = app();
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["first", "second"]);
    let window = app.registry.windows.get_mut(&child).unwrap();
    assert_eq!(window.tabs.active_index(), 1);
    let pane_id = window.tab_states[0].active_pane;
    window.panes.get_mut(&pane_id).unwrap().creator = PaneOwner::Agent(mine);
    let own = access(McpControl::Agent, McpIsolation::Shared, mine);
    assert!(matches!(app.mcp_focus_pane(&sessions, own, pane_id), LoopReply::Focused { .. }));
    assert_eq!(app.registry.windows[&child].tabs.active_index(), 0);
}

/// A tab zoomed on another pane refuses focus as `zoomed` and selects nothing; the zoomed pane
/// itself is still focusable, since the refusal spent no message slot.
#[test]
fn a_tab_zoomed_on_another_pane_refuses_focus() {
    let sessions = LiveSessions::default();
    let mine = sessions.open();
    let (mut app, _user, agent) = seeded(mine);
    let helper = split_headless(&mut app, agent, PaneOwner::Agent(mine));
    let main = app.main_mut().unwrap();
    assert!(main.tab_states[1].tree.toggle_zoom(agent));
    main.tabs.activate(0);
    let own = access(McpControl::Agent, McpIsolation::Shared, mine);
    assert_eq!(code(&app.mcp_focus_pane(&sessions, own, helper)), Some("zoomed"));
    assert_eq!(app.main().unwrap().tabs.active_index(), 0, "nothing is selected");
    assert_eq!(app.main().unwrap().tab_states[1].active_pane, agent);
    assert!(matches!(app.mcp_focus_pane(&sessions, own, agent), LoopReply::Focused { .. }));
    assert_eq!(app.main().unwrap().tabs.active_index(), 1);
}

/// The window is asked to take focus only while a SonicTerm window is frontmost; otherwise only
/// user attention is requested, so focus never moves away from another application.
#[test]
fn focus_raises_only_while_sonicterm_is_active() {
    let sessions = LiveSessions::default();
    let (first, second) = (sessions.open(), sessions.open());
    let (mut app, _user, agent) = seeded(first);
    take_raises();
    app.registry.frontmost_window = None;
    let inactive = access(McpControl::Agent, McpIsolation::Shared, first);
    assert!(matches!(app.mcp_focus_pane(&sessions, inactive, agent), LoopReply::Focused { .. }));
    assert_eq!(take_raises(), [RaiseRequest::Attention]);
    // A second session has its own message slot.
    app.registry.frontmost_window = app.registry.main_window_id;
    let active = access(McpControl::Agent, McpIsolation::Shared, second);
    // A headless window has no native window to raise, so the result reports none.
    let reply = app.mcp_focus_pane(&sessions, active, agent);
    assert!(matches!(reply, LoopReply::Focused { raised: false }));
    assert_eq!(take_raises(), [RaiseRequest::Focus]);
}

/// `focus_pane` and `notify` share one slot per session: after a focus, the same session's
/// message and next focus are `rate_limited` and show nothing, while another session's message
/// shows with SonicTerm's own prefix.
#[test]
fn one_session_gets_one_message_per_interval() {
    let sessions = LiveSessions::default();
    let (mine, theirs) = (sessions.open(), sessions.open());
    let (mut app, _user, agent) = seeded(mine);
    let own = access(McpControl::Agent, McpIsolation::Shared, mine);
    assert!(matches!(app.mcp_focus_pane(&sessions, own, agent), LoopReply::Focused { .. }));
    assert_eq!(code(&app.mcp_notify(&sessions, own, agent, "hi")), Some("rate_limited"));
    assert_eq!(code(&app.mcp_focus_pane(&sessions, own, agent)), Some("rate_limited"));
    assert!(app.main().unwrap().notification.is_none());
    let other = access(McpControl::Agent, McpIsolation::Shared, theirs);
    assert!(matches!(app.mcp_notify(&sessions, other, agent, "hi"), LoopReply::Notified));
    let expected = format!("Agent message (pane {agent}): hi");
    assert_eq!(app.__test_main_notification_message(), Some(expected.as_str()));
}

/// `set_title` sets the tab's custom title as the rename command does, and a blank title clears
/// it.
#[test]
fn set_title_sets_and_clears_the_tab_title() {
    let sessions = LiveSessions::default();
    let mine = sessions.open();
    let (mut app, _user, agent) = seeded(mine);
    let own = access(McpControl::Agent, McpIsolation::Shared, mine);
    let reply = app.mcp_set_title(own, agent, "tests: 3 failing");
    assert!(matches!(reply, LoopReply::Titled(Some(ref title)) if title == "tests: 3 failing"));
    let custom = |app: &App| app.main().unwrap().tabs.tabs()[1].custom_title.clone();
    assert_eq!(custom(&app).as_deref(), Some("tests: 3 failing"));
    assert!(matches!(app.mcp_set_title(own, agent, "   "), LoopReply::Titled(None)));
    assert_eq!(custom(&app), None);
}

/// Under `"agent"` a tab that also holds a pane the session may not control keeps its title, so
/// an agent split into a user's tab, or another session's under `isolation = "session"`, cannot
/// rename it; under `"any"` it can.
#[test]
fn set_title_is_refused_on_a_tab_shared_with_a_pane_out_of_scope() {
    let sessions = LiveSessions::default();
    let mine = sessions.open();
    let (mut app, _user, agent) = seeded(mine);
    let user_split = split_headless(&mut app, agent, PaneOwner::User);
    let own = access(McpControl::Agent, McpIsolation::Shared, mine);
    assert_eq!(code(&app.mcp_set_title(own, agent, "mine")), Some("out_of_scope"));
    assert_eq!(app.main().unwrap().tabs.tabs()[1].custom_title, None);
    let any = access(McpControl::Any, McpIsolation::Shared, mine);
    assert!(matches!(app.mcp_set_title(any, agent, "mine"), LoopReply::Titled(Some(_))));
    // Another session's helper in the tab refuses the rename under session isolation.
    app.main_mut().unwrap().panes.get_mut(&user_split).unwrap().creator =
        PaneOwner::Agent(sessions.open());
    let isolated = access(McpControl::Agent, McpIsolation::Session, mine);
    assert_eq!(code(&app.mcp_set_title(isolated, agent, "x")), Some("out_of_scope"));
    let shared = access(McpControl::Agent, McpIsolation::Shared, mine);
    assert!(matches!(app.mcp_set_title(shared, agent, "x"), LoopReply::Titled(Some(_))));
}

/// Display text loses every control character and the invisible format characters that reorder
/// or hide text (bidi overrides, zero-width marks, BOM, soft hyphen), then is capped by
/// characters, never splitting one.
#[test]
fn display_text_drops_control_and_format_characters_then_caps_characters() {
    let text = "a\u{202E}b\u{1b}[2J\u{200B}c\u{FEFF}\u{AD}\u{2066}d\u{7f}\u{85}e\ny";
    assert_eq!(display_text(text, 64), "ab[2Jcdey");
    assert_eq!(display_text(&"é".repeat(100), TITLE_MAX_CHARS).chars().count(), 64);
    assert_eq!(display_text("\u{200B}\u{1b}", 64), "");
}

/// Through the tool path a message carries SonicTerm's prefix even from a client that names
/// itself "SonicTerm", loses its control and format characters, is capped at 200 characters and
/// frees its notice slot once shown; a title is sanitized and capped at 64 characters.
#[test]
fn a_tool_path_message_carries_the_server_prefix_whatever_the_client_is_named() {
    let mut app = app();
    let pane = app.__test_seed_tab("agent");
    app.main_mut().unwrap().panes.get_mut(&pane).unwrap().creator =
        PaneOwner::Agent(AgentSessionId::next());
    let shared = install(&mut app, McpControl::Agent);
    let text = format!("done\u{1b}[2J\u{202E}{}", "x".repeat(300));
    let arguments = json!({ "pane_id": pane.to_string(), "text": text });
    let result = call_through_loop(&mut app, &shared, "notify", arguments, "SonicTerm");
    assert_eq!(result["structuredContent"]["status"], "shown", "{result}");
    let message = app.__test_main_notification_message().unwrap().to_owned();
    let prefix = format!("Agent message (pane {pane}): ");
    let body = message.strip_prefix(&prefix).unwrap_or_else(|| panic!("{message}"));
    assert!(body.starts_with("done[2Jxx"), "{body}");
    assert_eq!(body.chars().count(), NOTIFY_MAX_CHARS);
    assert_eq!(shared.pending_notices.load(Ordering::Acquire), 0, "the slot is freed once shown");
    let title = format!("\u{202E}{}", "t".repeat(100));
    let arguments = json!({ "pane_id": pane.to_string(), "title": title });
    let titled = call_through_loop(&mut app, &shared, "set_title", arguments, "SonicTerm");
    assert_eq!(titled["structuredContent"]["title"], "t".repeat(TITLE_MAX_CHARS), "{titled}");
}

/// A message while the notice queue is full is refused as `queue_full` before anything is
/// queued; once the queue drains, a message is shown again.
#[test]
fn a_full_notice_queue_refuses_a_message_before_it_is_queued() {
    let mut app = app();
    let pane = app.__test_seed_tab("agent");
    app.main_mut().unwrap().panes.get_mut(&pane).unwrap().creator =
        PaneOwner::Agent(AgentSessionId::next());
    let shared = install(&mut app, McpControl::Agent);
    for index in 0..MAX_PENDING_NOTICES {
        shared.notify(format!("notice {index}"));
    }
    let scope = CallScope::admit(&shared, &Arc::new(AtomicBool::new(false)));
    let arguments = json!({ "pane_id": pane.to_string(), "text": "hello" });
    let refused = tools_control::call(&shared, "notify", &arguments, &scope).unwrap();
    assert_eq!(refused["structuredContent"]["error"], "queue_full", "{refused}");
    assert_eq!(shared.inbox.lock().len() as u64, MAX_PENDING_NOTICES, "nothing more was queued");
    // The drain yields after its budget, so it runs until every notice was shown.
    while !shared.inbox.lock().is_empty() {
        app.drain_mcp_requests();
    }
    let shown = call_through_loop(&mut app, &shared, "notify", arguments, "agent");
    assert_eq!(shown["structuredContent"]["status"], "shown", "{shown}");
}
