//! Pins when agent connections reach the user: a probe that only initializes and lists tools is
//! silent, the first admitted tool call announces once and a refused one does not, a full notice
//! queue leaves the connection unannounced, and a departure is shown only while the session still
//! owns panes when the loop handles it.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::json;
use sonicterm_cfg::config::{Config, McpConfig, McpControl};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

use super::super::agents::{AgentSessionId, PaneOwner};
use super::super::mcp_read_scope_tests::exchange;
use super::super::mcp_tests::{call, connection, tool, wait_for_connections, Client};
use super::super::{spawn_connection, McpServer, McpShared, MAX_PENDING_NOTICES};
use crate::app::App;

/// An App with one user pane and a running server at `control = "agent"`.
fn app_with_server() -> (App, Arc<McpShared>, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let user = app.__test_seed_tab("user");
    app.config.mcp = McpConfig { enabled: true, ..McpConfig::default() };
    let shared = McpShared::new(McpControl::Agent, None);
    let owner = app.register_mcp_owner();
    app.services.mcp_server = Some(McpServer {
        shared: Arc::clone(&shared),
        endpoint: "test-endpoint".into(),
        _endpoint_guard: None,
        config: app.config.mcp,
        charges: std::collections::HashMap::new(),
        owner,
        read_notice_due: false,
    });
    (app, shared, user)
}

/// A connected client that introduced itself as `name`.
fn introduced(shared: &Arc<McpShared>, name: &str) -> Client {
    let (mut client, transport) = connection();
    spawn_connection(shared, transport);
    client.send(&call(0, "initialize", json!({ "clientInfo": { "name": name } })));
    client.reply();
    client
}

/// The text of the main window's notification, if one shows.
fn shown(app: &App) -> Option<String> {
    app.main().and_then(|main| main.notification.as_ref()).map(|bubble| bubble.message.clone())
}

/// The notices waiting in the inbox, as their text.
fn queued_notices(shared: &McpShared) -> Vec<String> {
    shared
        .inbox
        .lock()
        .iter()
        .filter_map(|request| match &request.call {
            super::super::LoopCall::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// A probe (`claude mcp list`, a tool discovery) initializes, lists tools and leaves: nothing is
/// shown, on arrival or departure, and no notice slot stays taken.
#[test]
fn a_probe_is_silent() {
    let (mut app, shared, _user) = app_with_server();
    let mut client = introduced(&shared, "claude");
    client.send(&call(1, "tools/list", json!({})));
    client.reply();
    client.hang_up();
    assert!(wait_for_connections(&shared, 0), "the probe's connection ended");
    app.drain_mcp_requests();
    assert_eq!(shown(&app), None);
    assert_eq!(shared.pending_notices.load(Ordering::Acquire), 0, "no slot left taken");
    shared.stop();
}

/// The first admitted tool call announces the client once, by the name `initialize` gave; a call
/// refused before admission (no tool name) announces nothing, and later calls add nothing.
#[test]
fn the_first_admitted_call_announces_once() {
    let (mut app, shared, _user) = app_with_server();
    let mut client = introduced(&shared, "claude");
    client.send(&call(1, "tools/call", json!({ "arguments": {} })));
    client.reply();
    assert!(queued_notices(&shared).is_empty(), "a refused call is not announced");
    // An unknown tool is admitted and answered on the worker without the loop.
    for id in 2..4 {
        client.send(&tool(id, "no_such_tool", json!({})));
        client.reply();
    }
    assert_eq!(queued_notices(&shared), ["Agent connected: claude"]);
    app.drain_mcp_requests();
    assert_eq!(shown(&app).as_deref(), Some("Agent connected: claude"));
    assert!(shared.sessions.snapshot()[0].announced);
    shared.stop();
}

/// With `MAX_PENDING_NOTICES` already waiting, the first call's notice is dropped and the
/// connection stays unannounced; it is never announced by a later call either.
#[test]
fn a_full_notice_queue_leaves_the_connection_unannounced() {
    let (mut app, shared, _user) = app_with_server();
    let mut client = introduced(&shared, "claude");
    for index in 0..MAX_PENDING_NOTICES {
        assert!(shared.notify(format!("busy {index}")));
    }
    client.send(&tool(1, "no_such_tool", json!({})));
    client.reply();
    assert_eq!(queued_notices(&shared).len() as u64, MAX_PENDING_NOTICES);
    assert!(!shared.sessions.snapshot()[0].announced);
    app.drain_mcp_requests();
    client.send(&tool(2, "no_such_tool", json!({})));
    client.reply();
    assert!(queued_notices(&shared).is_empty(), "only the first call announces");
    shared.stop();
}

/// An agent that leaves while it still owns a pane it created is shown leaving, with the count;
/// the departure's notice slot is freed once it is handled.
#[test]
fn a_departure_with_owned_panes_shows_the_count() {
    let (mut app, shared, user) = app_with_server();
    let client = introduced(&shared, "claude");
    let arguments = json!({ "anchor_pane_id": user.to_string(), "placement": "tab" });
    let (reply, mut client) = exchange(&mut app, client, tool(1, "create_pane", arguments));
    assert!(reply["result"]["structuredContent"]["pane_id"].is_string(), "{reply}");
    app.main_mut().unwrap().notification = None;
    client.hang_up();
    assert!(wait_for_connections(&shared, 0));
    app.drain_mcp_requests();
    assert_eq!(
        shown(&app).as_deref(),
        Some("Agent disconnected: claude; 1 pane(s) left without an agent")
    );
    assert_eq!(shared.pending_notices.load(Ordering::Acquire), 0);
    shared.stop();
}

/// The loop counts the panes the session owns when it handles the departure: none (never
/// created, or another session claimed them first) shows nothing; a pane still owned is counted.
#[test]
fn a_departure_counts_panes_owned_when_the_loop_handles_it() {
    let (mut app, _shared, user) = app_with_server();
    let (left, claimer) = (AgentSessionId::next(), AgentSessionId::next());
    app.mcp_agent_left(left, Some("claude"));
    assert_eq!(shown(&app), None, "no panes");
    app.registry.pane_mut(user).unwrap().creator = PaneOwner::Agent(claimer);
    app.mcp_agent_left(left, Some("claude"));
    assert_eq!(shown(&app), None, "claimed by another session first");
    app.mcp_agent_left(claimer, None);
    assert_eq!(
        shown(&app).as_deref(),
        Some("Agent disconnected: unknown; 1 pane(s) left without an agent")
    );
}

/// A connect notice holds a notice slot like every other notice: one still queued when the server
/// stops is dropped with the inbox, and dropping it frees its slot, so no place stays taken.
#[test]
fn a_connect_notice_dropped_at_stop_frees_its_slot() {
    let shared = McpShared::new(McpControl::Agent, None);
    assert!(shared.notify("Agent connected: claude".into()));
    assert_eq!(shared.pending_notices.load(Ordering::Acquire), 1);
    shared.stop();
    drop(shared.take_requests());
    assert_eq!(shared.pending_notices.load(Ordering::Acquire), 0, "the dropped notice freed it");
}
