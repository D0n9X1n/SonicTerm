//! Pins `[mcp] isolation` and `create_anchor` at the service boundary: two real connections a
//! real App answers. Each connection is its own agent session; under `"session"` one cannot
//! drive or read the other's panes, under `"shared"` it can, `list_panes` names each pane's
//! owner from the caller's side, and a pane outlives its session as `orphaned`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sonicterm_cfg::config::{
    Config, McpConfig, McpControl, McpCreateAnchor, McpIsolation, McpRead,
};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

use super::mcp_read_scope_tests::{exchange, refusal};
use super::mcp_tests::{connection, tool, Client};
use super::*;
use crate::app::App;

/// An App whose server runs at `read = "agent"`, `control = "agent"` and the given `isolation`
/// and `create_anchor`, with one user pane, and two connected clients, A and B.
fn two_agents(
    isolation: McpIsolation,
    create_anchor: McpCreateAnchor,
) -> (App, Arc<McpShared>, Client, Client, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let user = app.__test_seed_tab("user");
    app.config.mcp = McpConfig {
        enabled: true,
        read: Some(McpRead::Agent),
        isolation,
        create_anchor,
        ..McpConfig::default()
    };
    let shared = McpShared::new(McpControl::Agent, None);
    shared.set_read(McpRead::Agent);
    shared.set_agent_scope(isolation, create_anchor);
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
    let (first, first_transport) = connection();
    spawn_connection(&shared, first_transport);
    let (second, second_transport) = connection();
    spawn_connection(&shared, second_transport);
    (app, shared, first, second, user)
}

/// `create_pane` beside `anchor` on `client`; the new pane's id, or the refusal code.
fn create(app: &mut App, client: Client, anchor: u64) -> (Result<u64, String>, Client) {
    let request =
        tool(1, "create_pane", json!({ "anchor_pane_id": anchor.to_string(), "placement": "tab" }));
    let (reply, client) = exchange(app, client, request);
    let created = match refusal(&reply) {
        Some(code) => Err(code),
        None => Ok(reply["result"]["structuredContent"]["pane_id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .expect("a created pane id")),
    };
    (created, client)
}

/// The `owner` `list_panes` on `client` reports for each listed pane, by id.
fn owners(app: &mut App, client: Client) -> (Vec<(u64, String)>, Client) {
    let (reply, client) = exchange(app, client, tool(2, "list_panes", json!({})));
    let panes =
        reply["result"]["structuredContent"]["panes"].as_array().cloned().unwrap_or_default();
    let owners = panes
        .iter()
        .map(|pane: &Value| {
            let id = pane["pane_id"].as_str().unwrap().parse().unwrap();
            (id, pane["owner"].as_str().unwrap_or("missing").to_owned())
        })
        .collect();
    (owners, client)
}

/// `send_text` to `pane` on `client`; the refusal code, or `None` when it was written.
fn send(app: &mut App, client: Client, pane: u64) -> (Option<String>, Client) {
    let request = tool(3, "send_text", json!({ "pane_id": pane.to_string(), "text": "x" }));
    let (reply, client) = exchange(app, client, request);
    (refusal(&reply), client)
}

/// Under `"session"`, A's pane is A's alone: B neither lists, reads, nor writes it, and A does.
/// After A disconnects its pane stays open, orphaned, and no session may drive it.
#[test]
fn session_isolation_keeps_each_agents_panes_its_own() {
    let (mut app, shared, first, second, user) =
        two_agents(McpIsolation::Session, McpCreateAnchor::Any);
    let (created, first) = create(&mut app, first, user);
    let pane = created.expect("A opens a helper beside the user's pane");
    let (listed, first) = owners(&mut app, first);
    assert_eq!(listed, vec![(pane, "you".to_owned())], "A sees its own pane");
    let (listed, second) = owners(&mut app, second);
    assert!(listed.is_empty(), "B sees nothing of A's under \"session\": {listed:?}");
    let (refused, second) = send(&mut app, second, pane);
    assert_eq!(refused.as_deref(), Some("out_of_scope"));
    let read = tool(4, "read_pane", json!({ "pane_id": pane.to_string() }));
    let (reply, second) = exchange(&mut app, second, read);
    assert_eq!(refusal(&reply).as_deref(), Some("out_of_scope"));
    // A's own write is not refused for scope (a headless pane has no PTY to write to).
    let (refused, mut first) = send(&mut app, first, pane);
    assert_ne!(refused.as_deref(), Some("out_of_scope"));
    // A hangs up: the pane stays, and B still cannot reach it.
    first.hang_up();
    wait_for_connections(&shared, 1);
    assert!(app.registry.pane(pane).is_some(), "the pane outlives its session");
    let (refused, _second) = send(&mut app, second, pane);
    assert_eq!(refused.as_deref(), Some("out_of_scope"));
    shared.stop();
}

/// Under `"shared"` (the default) B reaches A's pane, sees it as `other_agent` while A is
/// connected and as `orphaned` once A has gone, and may still drive it.
#[test]
fn shared_isolation_lets_every_agent_reach_every_agent_pane() {
    let (mut app, shared, first, second, user) =
        two_agents(McpIsolation::Shared, McpCreateAnchor::Any);
    let (created, mut first) = create(&mut app, first, user);
    let pane = created.expect("A opens a helper");
    let (listed, second) = owners(&mut app, second);
    assert_eq!(listed, vec![(pane, "other_agent".to_owned())]);
    let (refused, second) = send(&mut app, second, pane);
    assert_ne!(refused.as_deref(), Some("out_of_scope"));
    first.hang_up();
    wait_for_connections(&shared, 1);
    let (listed, second) = owners(&mut app, second);
    assert_eq!(listed, vec![(pane, "orphaned".to_owned())]);
    let (refused, _second) = send(&mut app, second, pane);
    assert_ne!(refused.as_deref(), Some("out_of_scope"), "orphans stay shared");
    shared.stop();
}

/// `create_anchor = "agent"` refuses a helper beside the user's pane and allows one beside the
/// agent's own; the default `"any"` allowed the user's anchor in the tests above.
#[test]
fn create_anchor_agent_refuses_the_users_pane_as_anchor() {
    let (mut app, shared, first, _second, user) =
        two_agents(McpIsolation::Shared, McpCreateAnchor::Agent);
    let (created, first) = create(&mut app, first, user);
    assert_eq!(created, Err("out_of_scope".to_owned()));
    // A pane an agent created is a valid anchor.
    let agent_pane = app.__test_seed_tab("agent");
    app.registry.pane_mut(agent_pane).unwrap().creator = agents::PaneOwner::agent_if(true);
    let (created, _first) = create(&mut app, first, agent_pane);
    assert!(created.is_ok(), "{created:?}");
    shared.stop();
}

/// Wait until `shared` serves `count` connections, so a hang-up has fully ended its session.
fn wait_for_connections(shared: &McpShared, count: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.connections.load(std::sync::atomic::Ordering::Acquire) > count {
        assert!(Instant::now() < deadline, "the connection ended");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A create queued by a session whose connection closed before the loop ran it is refused unrun:
/// no pane appears for a client that is gone.
#[test]
fn a_create_from_a_closed_connection_creates_nothing() {
    let (mut app, shared, _first, _second, user) =
        two_agents(McpIsolation::Shared, McpCreateAnchor::Any);
    let session = shared.sessions.open();
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    let panes_before =
        app.registry.windows.values().map(|window| window.panes.len()).sum::<usize>();
    shared.inbox.lock().push(LoopRequest {
        call: LoopCall::CreatePane {
            anchor: user,
            placement: crate::app::pane_target::Placement::Tab,
            launch: Default::default(),
        },
        generation: shared.generation.load(std::sync::atomic::Ordering::Acquire),
        deadline: Instant::now() + Duration::from_secs(5),
        closed,
        session,
        reply,
    });
    shared.sessions.close(session);
    app.drain_mcp_requests();
    assert!(matches!(answer.try_recv(), Ok(LoopReply::Refused("revoked", _))));
    let panes_after = app.registry.windows.values().map(|window| window.panes.len()).sum::<usize>();
    assert_eq!(panes_after, panes_before, "a revoked create opened nothing");
    shared.stop();
}

/// A session that disconnects while its create is running still gets the pane, owned by its now
/// ended session: it reads as `orphaned` to the next agent and, under `"session"`, is out of reach.
#[test]
fn a_disconnect_during_a_create_leaves_an_orphaned_pane() {
    let (mut app, shared, _first, second, user) =
        two_agents(McpIsolation::Session, McpCreateAnchor::Any);
    let session = shared.sessions.open();
    let ending = Arc::clone(&shared);
    crate::app::pane_target::CREATE_HOOK
        .set(Some(Box::new(move || ending.sessions.close(session))));
    let reply = app.mcp_create_pane(
        shared.access(session),
        user,
        crate::app::pane_target::Placement::Tab,
        Default::default(),
    );
    crate::app::pane_target::CREATE_HOOK.set(None);
    let LoopReply::Created(created) = reply else { panic!("the create ran") };
    assert_eq!(
        app.registry.pane(created.pane_id).unwrap().creator,
        agents::PaneOwner::Agent(session)
    );
    assert!(!shared.sessions.contains(session), "the session ended mid-create");
    // Another agent sees nothing of it under "session", and cannot drive it.
    let (listed, second) = owners(&mut app, second);
    assert!(listed.iter().all(|(pane, _)| *pane != created.pane_id), "{listed:?}");
    let (refused, _second) = send(&mut app, second, created.pane_id);
    assert_eq!(refused.as_deref(), Some("out_of_scope"));
    // The ownership view a "shared" reader would get names it orphaned.
    let live = |id| shared.sessions.contains(id);
    let view =
        agents::OwnerView::of(agents::PaneOwner::Agent(session), shared.sessions.open(), live);
    assert_eq!(view, agents::OwnerView::Orphaned);
    shared.stop();
}
