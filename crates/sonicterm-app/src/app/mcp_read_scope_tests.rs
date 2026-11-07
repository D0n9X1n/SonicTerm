//! Pins `[mcp] read` at the service boundary, end to end: a real connection whose requests a real
//! App answers. Under `read = "agent"` the read tools see only agent-created panes, and pane
//! identity comes only from SonicTerm's own records: a forged or repeated bridge preamble naming
//! a user's pane gains nothing. Under `"any"` every pane stays readable, and tightening the scope
//! on reload revokes a running wait.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sonicterm_cfg::config::{Config, McpConfig, McpControl, McpRead};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

use super::mcp_tests::{call, connection, tool, Client};
use super::*;
use crate::app::App;

/// An App with a running server at `read`, one user pane and one agent-created pane, a connected
/// client, and the two pane ids (user, agent).
fn app_with_client(read: McpRead) -> (App, Arc<McpShared>, Client, u64, u64) {
    app_with_client_under(read, McpControl::Agent)
}

/// [`app_with_client`] with an explicit `control` scope.
fn app_with_client_under(
    read: McpRead,
    control: McpControl,
) -> (App, Arc<McpShared>, Client, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let user = app.__test_seed_tab("user");
    let agent = app.__test_seed_tab("agent");
    app.main_mut().unwrap().panes.get_mut(&agent).unwrap().agent_owned = true;
    app.config.mcp = McpConfig { enabled: true, control, read: Some(read) };
    let shared = McpShared::new(control, None);
    shared.set_read(read);
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
    let (client, transport) = connection();
    spawn_connection(&shared, transport);
    (app, shared, client, user, agent)
}

/// Send `request` on `client` from a helper thread and let `app` answer the loop requests it
/// queues until the reply arrives; returns the reply and the client back.
fn exchange(app: &mut App, mut client: Client, request: Value) -> (Value, Client) {
    let worker = std::thread::spawn(move || {
        client.send(&request);
        let reply = client.reply();
        (reply, client)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "no reply");
        app.drain_mcp_requests();
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.join().unwrap()
}

/// The pane ids a `list_panes` reply carries.
fn listed(reply: &Value) -> Vec<String> {
    reply["result"]["structuredContent"]["panes"]
        .as_array()
        .map(|panes| {
            panes.iter().map(|pane| pane["pane_id"].as_str().unwrap().to_owned()).collect()
        })
        .unwrap_or_default()
}

/// The tool error code in a reply, if the call was refused.
fn refusal(reply: &Value) -> Option<String> {
    reply["result"]["structuredContent"]["error"].as_str().map(str::to_owned)
}

#[test]
fn read_agent_limits_every_read_tool_to_agent_panes() {
    // Under "agent" the list shows only the agent's pane, and reading or waiting on the user's
    // pane is refused as out of scope while the agent's pane still reads.
    let (mut app, shared, client, user, agent) = app_with_client(McpRead::Agent);
    let (reply, client) = exchange(&mut app, client, tool(1, "list_panes", json!({})));
    assert_eq!(listed(&reply), vec![agent.to_string()]);
    assert_eq!(reply["result"]["structuredContent"]["read"], "agent");
    let read = |id: u64, pane: u64| tool(id, "read_pane", json!({ "pane_id": pane.to_string() }));
    let (reply, client) = exchange(&mut app, client, read(2, user));
    assert_eq!(refusal(&reply).as_deref(), Some("out_of_scope"));
    let (reply, client) = exchange(&mut app, client, read(3, agent));
    assert_eq!(refusal(&reply), None, "the agent's own pane reads: {reply}");
    let wait = tool(4, "wait_for", json!({ "pane_id": user.to_string(), "timeout_ms": 100 }));
    let (reply, _client) = exchange(&mut app, client, wait);
    assert_eq!(refusal(&reply).as_deref(), Some("out_of_scope"));
    shared.stop();
}

#[test]
fn a_forged_or_repeated_preamble_widens_nothing() {
    // The bridge preamble is the client's own claim. Naming the user's pane, first or again
    // later on the same connection, neither lists nor unlocks it under "agent".
    let (mut app, shared, mut client, user, agent) = app_with_client(McpRead::Agent);
    client.send(&json!({ "sonicterm_bridge": 1, "pane_id": user.to_string() }));
    let (reply, mut client) = exchange(&mut app, client, tool(1, "list_panes", json!({})));
    assert_eq!(listed(&reply), vec![agent.to_string()]);
    assert!(reply["result"]["structuredContent"]["self_pane_id"].is_null(), "{reply}");
    client.send(&json!({ "sonicterm_bridge": 1, "pane_id": user.to_string() }));
    let read = tool(2, "read_pane", json!({ "pane_id": user.to_string() }));
    let (reply, _client) = exchange(&mut app, client, read);
    assert_eq!(refusal(&reply).as_deref(), Some("out_of_scope"));
    shared.stop();
}

#[test]
fn read_any_keeps_every_pane_readable_and_resources_stay_unsupported() {
    // "any" is today's behavior: both panes list and read. Resource reads are not a way around
    // the scope, because the server offers no resources at all.
    let (mut app, shared, client, user, agent) = app_with_client(McpRead::Any);
    let (reply, client) = exchange(&mut app, client, tool(1, "list_panes", json!({})));
    let mut ids = listed(&reply);
    ids.sort();
    let mut expected = vec![user.to_string(), agent.to_string()];
    expected.sort();
    assert_eq!(ids, expected);
    let read = tool(2, "read_pane", json!({ "pane_id": user.to_string() }));
    let (reply, client) = exchange(&mut app, client, read);
    assert_eq!(refusal(&reply), None, "{reply}");
    let resource = call(3, "resources/read", json!({ "uri": format!("sonicterm://pane/{user}") }));
    let (reply, _client) = exchange(&mut app, client, resource);
    assert!(reply["error"].is_object(), "resources are refused: {reply}");
    shared.stop();
}

#[test]
fn tightening_read_on_reload_revokes_a_running_wait() {
    // A wait on the user's pane under "any" ends as revoked once a reload sets read = "agent",
    // so a narrowed scope never keeps streaming a pane it no longer allows.
    let (mut app, shared, mut client, user, _agent) = app_with_client(McpRead::Any);
    let wait = tool(
        1,
        "wait_for",
        json!({
            "pane_id": user.to_string(), "regex": "NEVER", "timeout_ms": 8_000
        }),
    );
    let worker = std::thread::spawn(move || {
        client.send(&wait);
        client.reply()
    });
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(300) {
        app.drain_mcp_requests();
        std::thread::sleep(Duration::from_millis(5));
    }
    app.config.mcp.read = Some(McpRead::Agent);
    app.reconcile_mcp();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "the wait outlived the narrowed scope");
        app.drain_mcp_requests();
        std::thread::sleep(Duration::from_millis(5));
    }
    let reply = worker.join().unwrap();
    let text = reply.to_string();
    assert!(text.contains("revoked"), "{reply}");
    shared.stop();
}

#[test]
fn changing_only_the_read_scope_revokes_queued_requests() {
    // `set_read` alone bumps the generation, so a request admitted before the change is
    // refused as revoked even when `control` is untouched.
    let shared = McpShared::new(McpControl::Agent, None);
    let before = shared.generation.load(std::sync::atomic::Ordering::Acquire);
    shared.set_read(McpRead::Agent);
    assert_ne!(shared.generation.load(std::sync::atomic::Ordering::Acquire), before);
    assert_eq!(shared.read(), McpRead::Agent);
    shared.set_read(McpRead::Any);
    assert_eq!(shared.read(), McpRead::Any);
}

#[test]
fn every_read_tool_answers_each_pane_by_ownership_and_scope() {
    // The full matrix: list_panes, read_pane and wait_for, on the user's pane and the agent's,
    // under "agent" and "any". Each permitted call succeeds (a quiet wait ends `quiet`, never
    // refused); each forbidden one is refused as out_of_scope, so neither half can pass alone.
    for read in [McpRead::Agent, McpRead::Any] {
        let (mut app, shared, mut client, user, agent) = app_with_client(read);
        for (pane, owned) in [(user, false), (agent, true)] {
            let permitted = read == McpRead::Any || owned;
            let label = format!("{read:?} pane owned={owned}");
            let (reply, next) = exchange(&mut app, client, tool(1, "list_panes", json!({})));
            assert_eq!(listed(&reply).contains(&pane.to_string()), permitted, "{label}: list");
            let read_pane = tool(2, "read_pane", json!({ "pane_id": pane.to_string() }));
            let (reply, next) = exchange(&mut app, next, read_pane);
            let expected = (!permitted).then(|| "out_of_scope".to_owned());
            assert_eq!(refusal(&reply), expected, "{label}: read_pane {reply}");
            let wait = json!({ "pane_id": pane.to_string(), "quiet_ms": 50, "timeout_ms": 2_000 });
            let (reply, next) = exchange(&mut app, next, tool(3, "wait_for", wait));
            assert_eq!(refusal(&reply), expected, "{label}: wait_for {reply}");
            if permitted {
                assert_eq!(reply["result"]["structuredContent"]["reason"], "quiet", "{label}");
            }
            client = next;
        }
        shared.stop();
    }
}

#[test]
fn run_command_follows_control_not_read() {
    // run_command returns the output of the command it ran, a control tool's result, so under
    // read = "agent" with control = "any" it is never refused as out of scope on a user's pane;
    // the scope it answers to is control. (Without a live PTY the run then fails for that reason.)
    let (mut app, shared, client, user, _agent) =
        app_with_client_under(McpRead::Agent, McpControl::Any);
    let run = tool(1, "run_command", json!({ "pane_id": user.to_string(), "command": "true" }));
    let (reply, client) = exchange(&mut app, client, run);
    assert_ne!(refusal(&reply).as_deref(), Some("out_of_scope"), "{reply}");
    let read = tool(2, "read_pane", json!({ "pane_id": user.to_string() }));
    let (reply, _client) = exchange(&mut app, client, read);
    assert_eq!(
        refusal(&reply).as_deref(),
        Some("out_of_scope"),
        "the read tools still follow read"
    );
    shared.stop();
}
