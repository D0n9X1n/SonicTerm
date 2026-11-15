//! Pins the Agents page's disconnect over real connections: an idle connection closes with no
//! notice, a queued mutation is refused, a screen wait and a stream wait end `revoked`, a running
//! `run_command` returns `revoked` while SonicTerm sends its command nothing, a second connection
//! keeps working, and a claim is refused `session_live` until the disconnected session tears down.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::json;
use sonicterm_cfg::config::McpControl;

use super::super::mcp_tests::{
    call, connection, fake_loop, sleeping_wait, sleeping_wait_with, tool, wait_ended,
    wait_for_connections, Client,
};
use super::super::{spawn_connection, test_entry, LoopCall, LoopReply, McpShared};
use super::*;

/// A connected, introduced client and its session, once the session is listed.
fn connected(shared: &Arc<McpShared>) -> (Client, AgentSessionId) {
    let before: Vec<AgentSessionId> =
        shared.sessions.snapshot().iter().map(|entry| entry.session).collect();
    let (mut client, transport) = connection();
    spawn_connection(shared, transport);
    client.send(&call(0, "initialize", json!({ "clientInfo": { "name": "agent" } })));
    client.reply();
    let session = shared
        .sessions
        .snapshot()
        .into_iter()
        .map(|entry| entry.session)
        .find(|session| !before.contains(session))
        .expect("the new connection's session is listed");
    (client, session)
}

/// The only session `shared` lists.
fn only_session(shared: &McpShared) -> AgentSessionId {
    let listed = shared.sessions.snapshot();
    assert_eq!(listed.len(), 1);
    listed[0].session
}

/// An idle connection closes at once: the client reads end of stream, its session leaves, and
/// because the user chose it no departure notice is queued; a second disconnect is a no-op.
#[test]
fn an_idle_connection_closes() {
    let shared = McpShared::new(McpControl::Agent, None);
    let (mut client, session) = connected(&shared);
    assert!(shared.disconnect_agent(session));
    assert!(client.sees_eof(), "the client sees the server close");
    assert!(wait_for_connections(&shared, 0));
    assert!(!shared.sessions.contains(session));
    assert!(shared.inbox.lock().is_empty(), "a user disconnect queues no notice");
    assert!(!shared.disconnect_agent(session), "already gone");
    shared.stop();
}

/// A mutation queued for a loop that has not answered yet is refused `revoked`, and the request
/// the loop later finds is marked closed, so it is dropped unrun.
#[test]
fn a_queued_mutation_is_refused() {
    let shared = McpShared::new(McpControl::Agent, None);
    let (mut client, session) = connected(&shared);
    client.send(&tool(1, "send_text", json!({ "pane_id": "7", "text": "rm -rf ~" })));
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.inbox.lock().is_empty() {
        assert!(Instant::now() < deadline, "the call was queued");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(shared.disconnect_agent(session));
    let reply = client.reply();
    assert_eq!(reply["result"]["structuredContent"]["error"], "revoked", "{reply}");
    // The connect notice the call queued first is not a tool request.
    let queued: Vec<_> = shared
        .take_requests()
        .into_iter()
        .filter(|request| !matches!(request.call, LoopCall::Notice { .. }))
        .collect();
    assert_eq!(queued.len(), 1);
    assert!(queued[0].closed.load(Ordering::Acquire), "the loop drops it unrun");
    shared.stop();
}

/// A screen wait and a stream wait, each parked on its pane, end `revoked` at once, well before
/// their 10 s recheck, so only the disconnect's wake can have ended them.
#[test]
fn screen_and_stream_waits_end_revoked() {
    for extra in [json!({}), json!({ "source": "stream" })] {
        let (shared, mut client, looper) =
            if extra == json!({}) { sleeping_wait() } else { sleeping_wait_with(extra.clone()) };
        assert!(shared.disconnect_agent(only_session(&shared)));
        assert!(wait_ended(&shared), "{extra}: the wait outlived the disconnect");
        let reply = client.reply();
        assert_eq!(reply["result"]["structuredContent"]["error"], "revoked", "{extra}: {reply}");
        shared.stop();
        looper.join().unwrap();
    }
}

/// A running `run_command` returns `revoked`, and SonicTerm sends its pane nothing more (no close,
/// no keys, no text), so the command it already started keeps running in the shell.
#[test]
fn a_running_command_returns_revoked_and_keeps_running() {
    let shared = McpShared::new(McpControl::Agent, None);
    *shared.recheck_override.lock() = Some(Duration::from_secs(10));
    let entry = test_entry(40, 8, 100);
    entry.parser.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let seq = entry.parser.lock().grid().newest_prompt().unwrap().seq;
    assert!(entry.parser.lock().grid_mut().pin_run(seq));
    entry.agent_run.store(true, Ordering::Release);
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let looper = {
        let (shared, seen, entry) = (Arc::clone(&shared), Arc::clone(&seen), entry.clone());
        std::thread::spawn(move || {
            while !shared.is_stopped() {
                for request in shared.take_requests() {
                    let reply = match request.call {
                        // A connect or departure notice is not sent to the pane.
                        LoopCall::Notice { .. } | LoopCall::AgentLeft { .. } => continue,
                        LoopCall::RunStart { .. } => {
                            seen.lock().push("run_start");
                            let started =
                                crate::app::mcp::run::RunStarted { entry: entry.clone(), seq };
                            LoopReply::RunStarted(Box::new(started))
                        }
                        _ => {
                            seen.lock().push("other");
                            LoopReply::Refused("unavailable", "not in this test".into())
                        }
                    };
                    let _ = request.reply.try_send(reply);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let (mut client, session) = connected(&shared);
    client.send(&tool(1, "run_command", json!({ "pane_id": "7", "command": "make" })));
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.waiter_count() == 0 {
        assert!(Instant::now() < deadline, "the run is waiting");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(shared.disconnect_agent(session));
    let reply = client.reply();
    assert_eq!(reply["result"]["structuredContent"]["error"], "revoked", "{reply}");
    assert!(wait_for_connections(&shared, 0));
    assert_eq!(*seen.lock(), ["run_start"], "nothing else reached the pane");
    shared.stop();
    looper.join().unwrap();
}

/// Disconnecting one agent leaves another connected and answering; "all" then ends every
/// connection open at that moment.
#[test]
fn another_connection_is_unaffected_and_all_ends_every_one() {
    let shared = McpShared::new(McpControl::Agent, None);
    let looper = fake_loop(&shared, test_entry(20, 4, 100));
    let (mut first, first_session) = connected(&shared);
    let (mut second, _) = connected(&shared);
    let (mut third, _) = connected(&shared);
    assert!(shared.disconnect_agent(first_session));
    assert!(first.sees_eof());
    second.send(&tool(1, "list_panes", json!({})));
    assert_eq!(second.reply()["result"]["isError"], false, "the other agent still works");
    assert!(wait_for_connections(&shared, 2));
    assert_eq!(shared.disconnect_all_agents(), 2);
    assert!(second.sees_eof() && third.sees_eof());
    assert!(wait_for_connections(&shared, 0));
    shared.stop();
    looper.join().unwrap();
}

/// A session disconnected from the page keeps refusing claims as `session_live` while its call
/// slot is taken or its connection has not torn down; once it has, its token claims its panes.
#[test]
fn a_claim_waits_for_the_disconnected_session_to_tear_down() {
    let shared = McpShared::new(McpControl::Agent, None);
    let in_flight = Arc::new(AtomicBool::new(true));
    let source = shared
        .sessions
        .open_connection(Some(Arc::clone(&in_flight)), Arc::new(AtomicBool::new(false)));
    let caller = shared.sessions.open();
    let tokens = &shared.sessions.tokens;
    let (resume, own) = (tokens.token_of(source).unwrap(), tokens.token_of(caller).unwrap());
    let mut app = crate::app::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let pane = app.__test_seed_tab("helper");
    app.registry.pane_mut(pane).unwrap().creator = PaneOwner::Agent(source);
    let access = Access::from(McpControl::Agent);
    let access = Access { session: caller, ..access };
    let refused =
        |reply: LoopReply| matches!(reply, LoopReply::Refused(code, _) if code == "session_live");
    assert!(shared.disconnect_agent(source));
    assert!(refused(app.mcp_claim_panes(tokens, access, &resume, &own)), "before teardown");
    shared.sessions.close(source);
    assert!(refused(app.mcp_claim_panes(tokens, access, &resume, &own)), "a call still runs");
    in_flight.store(false, Ordering::Release);
    let claimed = app.mcp_claim_panes(tokens, access, &resume, &own);
    assert!(matches!(claimed, LoopReply::Claimed(1)), "after teardown");
    assert_eq!(app.registry.pane(pane).unwrap().creator, PaneOwner::Agent(caller));
}
