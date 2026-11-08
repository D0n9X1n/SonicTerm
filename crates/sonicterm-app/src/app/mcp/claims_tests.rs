//! Pins `claim_panes` and resume tokens: a reconnecting agent takes back its closed session's
//! panes through real connections and a real App; a live or undrained session refuses; a wrong
//! token of either kind moves nothing and reads the same; a claim deletes the source token, so
//! it never reaches panes the claimant owned before; a lost reply is recovered with the
//! claimant's own token; a revoking reload reissues tokens and a limit-only one keeps them; the
//! registry stays bounded; and no token reaches a log.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sonicterm_cfg::config::{Config, McpConfig, McpControl, McpIsolation, McpRead};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::copy_mode::CopyModeState;

use super::super::mcp_read_scope_tests::{exchange, refusal};
use super::super::mcp_tests::{connection, tool, Client};
use super::super::{spawn_connection, McpServer, McpShared};
use super::*;

/// An App whose server runs at `control = "agent"`, `read = "agent"` and `isolation =
/// "session"`, with one user pane to anchor helpers beside.
fn server() -> (App, Arc<McpShared>, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let user = app.__test_seed_tab("user");
    app.config.mcp = McpConfig {
        enabled: true,
        read: Some(McpRead::Agent),
        isolation: McpIsolation::Session,
        ..McpConfig::default()
    };
    let shared = McpShared::new(McpControl::Agent, None);
    shared.set_read(McpRead::Agent);
    shared.set_agent_scope(McpIsolation::Session, Default::default());
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

/// A new connected client.
fn connect(shared: &Arc<McpShared>) -> Client {
    let (client, transport) = connection();
    spawn_connection(shared, transport);
    client
}

/// Hang `client` up and wait until its connection ended, so its session is closed and drained.
fn disconnect(shared: &McpShared, mut client: Client) {
    let before = shared.connections.load(Ordering::Acquire);
    client.hang_up();
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.connections.load(Ordering::Acquire) >= before {
        assert!(Instant::now() < deadline, "the connection ended");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `name` with `arguments` on `client`; the reply's structured content.
fn call(app: &mut App, client: Client, name: &str, arguments: Value) -> (Value, Client) {
    let (reply, client) = exchange(app, client, tool(1, name, arguments));
    (reply["result"]["structuredContent"].clone(), client)
}

/// `client`'s own resume token.
fn token(app: &mut App, client: Client) -> (String, Client) {
    let (content, client) = call(app, client, "session_token", json!({}));
    (content["resume_token"].as_str().expect("a token").to_owned(), client)
}

/// A helper tab beside `anchor`, created by `client`; its id and the reply's resume token.
fn create(app: &mut App, client: Client, anchor: u64) -> (u64, Option<String>, Client) {
    let arguments = json!({ "anchor_pane_id": anchor.to_string(), "placement": "tab" });
    let (content, client) = call(app, client, "create_pane", arguments);
    let pane = content["pane_id"].as_str().and_then(|id| id.parse().ok()).expect("created");
    (pane, content["resume_token"].as_str().map(str::to_owned), client)
}

/// `claim_panes` on `client`; the claimed count, or the refusal code.
fn claim(app: &mut App, client: Client, resume: &str, own: &str) -> (Result<u64, String>, Client) {
    let arguments = json!({ "resume_token": resume, "own_token": own });
    let (reply, client) = exchange(app, client, tool(1, "claim_panes", arguments));
    let outcome = match refusal(&reply) {
        Some(code) => Err(code),
        None => Ok(reply["result"]["structuredContent"]["claimed"].as_u64().unwrap()),
    };
    (outcome, client)
}

/// The panes `client`'s `list_panes` names as its own.
fn own_panes(app: &mut App, client: Client) -> (HashSet<u64>, Client) {
    let (content, client) = call(app, client, "list_panes", json!({}));
    let panes = content["panes"].as_array().cloned().unwrap_or_default();
    let own = panes
        .iter()
        .filter(|pane| pane["owner"] == "you")
        .map(|pane| pane["pane_id"].as_str().unwrap().parse().unwrap())
        .collect();
    (own, client)
}

/// Create, disconnect, reconnect, claim: the new session owns the panes, the old token is spent,
/// and `create_pane` returned the same token `session_token` does.
#[test]
fn a_reconnected_agent_claims_its_panes() {
    let (mut app, shared, user) = server();
    let first = connect(&shared);
    let (session_token, first) = token(&mut app, first);
    let (pane, returned, first) = create(&mut app, first, user);
    assert_eq!(returned.as_deref(), Some(session_token.as_str()), "one canonical token");
    disconnect(&shared, first);
    let second = connect(&shared);
    let (own, second) = token(&mut app, second);
    let (claimed, second) = claim(&mut app, second, &session_token, &own);
    assert_eq!(claimed, Ok(1));
    let (listed, second) = own_panes(&mut app, second);
    assert_eq!(listed, HashSet::from([pane]));
    let (again, _second) = claim(&mut app, second, &session_token, &own);
    assert_eq!(again, Err("not_found".to_owned()), "a claimed token is spent");
    shared.stop();
}

/// A session still connected refuses the claim at once; a wrong token, or a wrong own token, is
/// `not_found` and moves nothing.
#[test]
fn a_live_session_or_a_wrong_token_refuses() {
    let (mut app, shared, user) = server();
    let first = connect(&shared);
    let (first_token, first) = token(&mut app, first);
    let (pane, _, first) = create(&mut app, first, user);
    let second = connect(&shared);
    let (own, second) = token(&mut app, second);
    let (refused, second) = claim(&mut app, second, &first_token, &own);
    assert_eq!(refused, Err("session_live".to_owned()));
    disconnect(&shared, first);
    let (refused, second) = claim(&mut app, second, &"0".repeat(32), &own);
    assert_eq!(refused, Err("not_found".to_owned()), "an unknown token");
    let (refused, second) = claim(&mut app, second, &first_token, &"0".repeat(32));
    assert_eq!(refused, Err("not_found".to_owned()), "the caller's own token is wrong");
    let (listed, _second) = own_panes(&mut app, second);
    assert!(listed.is_empty(), "nothing moved");
    assert_eq!(
        app.registry.pane(pane).unwrap().creator,
        PaneOwner::Agent(source_of(&app, pane)),
        "still the first session's"
    );
    shared.stop();
}

/// The session that created `pane`.
fn source_of(app: &App, pane: u64) -> AgentSessionId {
    match app.registry.pane(pane).unwrap().creator {
        PaneOwner::Agent(session) => session,
        PaneOwner::User => panic!("an agent pane"),
    }
}

/// A session cannot claim its own token: it names no other session, so it is `not_found`.
#[test]
fn a_session_cannot_claim_its_own_token() {
    let registry = ResumeRegistry::default();
    let caller = AgentSessionId::next();
    registry.connect(caller, None);
    let own = registry.token_of(caller).unwrap();
    assert_eq!(registry.take(caller, &own, &own), Err(ClaimRefusal::NotFound));
    assert_eq!(registry.token_of(caller), Some(own), "and keeps it");
}

/// A closed session with a call still running is not drained: the claim is refused until its
/// call slot clears.
#[test]
fn an_undrained_session_refuses() {
    let registry = ResumeRegistry::default();
    let slot = Arc::new(AtomicBool::new(true));
    let source = AgentSessionId::next();
    let caller = AgentSessionId::next();
    registry.connect(source, Some(Arc::clone(&slot)));
    registry.connect(caller, None);
    registry.disconnect(source);
    let (resume, own) = (registry.token_of(source).unwrap(), registry.token_of(caller).unwrap());
    assert_eq!(registry.take(caller, &resume, &own), Err(ClaimRefusal::SessionLive));
    slot.store(false, Ordering::Release);
    assert_eq!(registry.take(caller, &resume, &own), Ok(source));
}

/// Two claims with one token: exactly one wins, the other finds it spent.
#[test]
fn two_claims_with_one_token_have_one_winner() {
    let registry = ResumeRegistry::default();
    let source = AgentSessionId::next();
    let (left, right) = (AgentSessionId::next(), AgentSessionId::next());
    for session in [source, left, right] {
        registry.connect(session, None);
    }
    registry.disconnect(source);
    let resume = registry.token_of(source).unwrap();
    let outcomes = [left, right].map(|caller| {
        let own = registry.token_of(caller).unwrap();
        registry.take(caller, &resume, &own)
    });
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert!(outcomes.contains(&Err(ClaimRefusal::NotFound)));
}

/// A claimant that already owns panes keeps them, and the claimed token cannot reach them: it is
/// deleted, never an alias of the claimant's.
#[test]
fn a_claim_never_aliases_the_old_token() {
    let (mut app, shared, user) = server();
    let first = connect(&shared);
    let (first_token, first) = token(&mut app, first);
    let (claimed_pane, _, first) = create(&mut app, first, user);
    disconnect(&shared, first);
    let second = connect(&shared);
    let (own, second) = token(&mut app, second);
    let (earlier_pane, _, second) = create(&mut app, second, user);
    let (claimed, second) = claim(&mut app, second, &first_token, &own);
    assert_eq!(claimed, Ok(1));
    let (listed, second) = own_panes(&mut app, second);
    assert_eq!(listed, HashSet::from([claimed_pane, earlier_pane]));
    // A third session holding the old token reaches nothing, the claimant's earlier pane included.
    disconnect(&shared, second);
    let third = connect(&shared);
    let (third_own, third) = token(&mut app, third);
    let (refused, _third) = claim(&mut app, third, &first_token, &third_own);
    assert_eq!(refused, Err("not_found".to_owned()));
    shared.stop();
}

/// A reply lost after the commit: the claimer disconnects, and a later session claims everything
/// with the claimer's own token, which it read before claiming. Reconnecting twice works.
#[test]
fn a_lost_reply_is_recovered_with_the_claimers_token() {
    let (mut app, shared, user) = server();
    let first = connect(&shared);
    let (first_token, first) = token(&mut app, first);
    let (pane, _, first) = create(&mut app, first, user);
    disconnect(&shared, first);
    let second = connect(&shared);
    let (second_token, second) = token(&mut app, second);
    let (claimed, second) = claim(&mut app, second, &first_token, &second_token);
    assert_eq!(claimed, Ok(1));
    disconnect(&shared, second);
    let third = connect(&shared);
    let (third_token, third) = token(&mut app, third);
    let (claimed, third) = claim(&mut app, third, &second_token, &third_token);
    assert_eq!(claimed, Ok(1));
    let (listed, _third) = own_panes(&mut app, third);
    assert_eq!(listed, HashSet::from([pane]));
    shared.stop();
}

/// Twenty reconnect-and-claim cycles leave one record per live session plus one per closed
/// session that still owns panes.
#[test]
fn the_registry_stays_bounded_across_cycles() {
    let (mut app, shared, user) = server();
    let first = connect(&shared);
    let (mut held, first) = token(&mut app, first);
    let (_, _, first) = create(&mut app, first, user);
    disconnect(&shared, first);
    for _ in 0..20 {
        // An idle session that never created a pane is forgotten once pruned.
        let idle = connect(&shared);
        disconnect(&shared, idle);
        let next = connect(&shared);
        let (own, next) = token(&mut app, next);
        let (claimed, next) = claim(&mut app, next, &held, &own);
        assert_eq!(claimed, Ok(1));
        held = own;
        disconnect(&shared, next);
    }
    app.prune_resume_tokens(&shared.sessions.tokens);
    assert_eq!(shared.sessions.tokens.len(), 1, "only the last owner's record is kept");
    shared.stop();
}

/// A reload revoking `control` drops a closed session's token and reissues a live one's; a
/// reload changing only `max_connections` keeps both.
#[test]
fn a_revoking_reload_reissues_tokens_and_a_limit_reload_keeps_them() {
    let (mut app, shared, user) = server();
    let closed = connect(&shared);
    let (closed_token, closed) = token(&mut app, closed);
    let _ = create(&mut app, closed, user);
    let closed = connect(&shared);
    disconnect(&shared, closed);
    let live = connect(&shared);
    let (live_token, live) = token(&mut app, live);
    app.config.mcp.max_connections = 3;
    app.reconcile_mcp();
    let (kept, live) = token(&mut app, live);
    assert_eq!(kept, live_token, "a limit-only reload revokes nothing");
    app.config.mcp.control = McpControl::Any;
    app.reconcile_mcp();
    let (fresh, live) = token(&mut app, live);
    assert_ne!(fresh, live_token, "a revoking reload issues a fresh token");
    let (refused, _live) = claim(&mut app, live, &closed_token, &fresh);
    assert_eq!(refused, Err("not_found".to_owned()), "closed sessions' tokens were dropped");
    let closed_session = app
        .registry
        .windows
        .values()
        .flat_map(|window| window.panes.values())
        .find_map(|pane| match pane.creator {
            PaneOwner::Agent(session) => Some(session),
            PaneOwner::User => None,
        })
        .expect("the closed session's pane");
    assert_eq!(shared.sessions.tokens.token_of(closed_session), None, "and not re-minted");
    shared.stop();
}

/// A session whose token could not be minted runs without one: `session_token` is
/// `unavailable` and `create_pane` returns no `resume_token`.
#[test]
fn a_session_without_a_token_still_runs() {
    let (mut app, shared, user) = server();
    shared.sessions.tokens.fail_mint.store(true, Ordering::Relaxed);
    let client = connect(&shared);
    let (reply, client) = exchange(&mut app, client, tool(1, "session_token", json!({})));
    assert_eq!(refusal(&reply).as_deref(), Some("unavailable"));
    let (_, returned, _client) = create(&mut app, client, user);
    assert_eq!(returned, None);
    shared.stop();
}

/// Control off refuses the claim; READONLY on the pane's window does not, since only ownership
/// moves and every later write is still refused.
#[test]
fn control_off_refuses_and_readonly_does_not() {
    let (mut app, shared, _user) = server();
    let registry = &shared.sessions.tokens;
    let source = AgentSessionId::next();
    let caller = AgentSessionId::next();
    registry.connect(source, None);
    registry.connect(caller, None);
    registry.disconnect(source);
    let pane = app.__test_seed_tab("agent");
    app.registry.pane_mut(pane).unwrap().creator = PaneOwner::Agent(source);
    let (resume, own) = (registry.token_of(source).unwrap(), registry.token_of(caller).unwrap());
    let mut access = shared.access(caller);
    access.control = McpControl::Off;
    let refused = app.mcp_claim_panes(registry, access, &resume, &own);
    assert!(matches!(refused, LoopReply::Refused("control_off", _)));
    app.main_mut().unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
    assert!(app.pane_window_read_only(pane));
    let claimed = app.mcp_claim_panes(registry, shared.access(caller), &resume, &own);
    assert!(matches!(claimed, LoopReply::Claimed(1)), "READONLY does not block a claim");
    shared.stop();
}

/// No token reaches a log: a claim's own line carries the count, and a token's `Debug` is opaque.
#[test]
fn no_token_reaches_a_log() {
    let (mut app, shared, _user) = server();
    let registry = &shared.sessions.tokens;
    let source = AgentSessionId::next();
    let caller = AgentSessionId::next();
    registry.connect(source, None);
    registry.connect(caller, None);
    registry.disconnect(source);
    let pane = app.__test_seed_tab("agent");
    app.registry.pane_mut(pane).unwrap().creator = PaneOwner::Agent(source);
    let (resume, own) = (registry.token_of(source).unwrap(), registry.token_of(caller).unwrap());
    let log = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
    let writer = Arc::clone(&log);
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || SharedLog(Arc::clone(&writer)))
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, || {
        tracing::debug!(target: "mcp", ?resume, "probe");
        let reply = app.mcp_claim_panes(registry, shared.access(caller), &resume, &own);
        assert!(matches!(reply, LoopReply::Claimed(1)));
    });
    let text = String::from_utf8(log.lock().clone()).unwrap();
    assert!(text.contains("agent claimed panes"), "{text}");
    for secret in [resume.to_hex(), own.to_hex()] {
        assert!(!text.contains(&secret), "a token was logged: {text}");
    }
    assert!(text.contains("ResumeToken(..)"));
    shared.stop();
}

/// A capture buffer the test subscriber writes into.
struct SharedLog(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for SharedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
