//! Pins the agent-session rules on their own: ids never repeat, a session's `"agent"` scope
//! reaches every agent pane under `"shared"` and only its own under `"session"`, and a pane whose
//! session ended reads as orphaned.

use sonicterm_cfg::config::{McpControl, McpCreateAnchor, McpIsolation, McpRead};

use super::*;

/// The access a session `session` has at `isolation`, with agent control.
fn access(session: AgentSessionId, isolation: McpIsolation) -> Access {
    Access { control: McpControl::Agent, isolation, create_anchor: McpCreateAnchor::Any, session }
}

/// Ids only grow, so a session started after a server restart never takes an earlier pane's id.
#[test]
fn session_ids_are_never_reused() {
    let sessions = LiveSessions::default();
    let first = sessions.open();
    sessions.close(first);
    // A second set stands for a restarted server: the counter is process-wide.
    let restarted = LiveSessions::default();
    let second = restarted.open();
    assert!(second.get() > first.get());
    assert!(!restarted.contains(first));
}

/// Under `"shared"` any agent pane is reached, the user's never; under `"session"` only the
/// session's own, so another session's pane and an orphaned one are out of reach.
#[test]
fn isolation_decides_which_agent_panes_a_session_reaches() {
    let (mine, theirs) = (AgentSessionId::next(), AgentSessionId::next());
    for (isolation, other_reached) in [(McpIsolation::Shared, true), (McpIsolation::Session, false)]
    {
        let session = access(mine, isolation);
        assert!(session.reaches(PaneOwner::Agent(mine)), "{isolation:?}");
        assert_eq!(session.reaches(PaneOwner::Agent(theirs)), other_reached, "{isolation:?}");
        assert!(!session.reaches(PaneOwner::User), "{isolation:?}");
        // `read = "any"` sees every pane whatever the isolation.
        assert!(session.reads(McpRead::Any, PaneOwner::User));
        assert_eq!(session.reads(McpRead::Agent, PaneOwner::Agent(theirs)), other_reached);
    }
}

/// The owner a session sees: its own panes are `you`, a connected session's `other_agent`, an
/// ended session's `orphaned`, and the user's `user`.
#[test]
fn owner_view_follows_the_live_set() {
    let sessions = LiveSessions::default();
    let (mine, theirs) = (sessions.open(), sessions.open());
    let live = |session| sessions.contains(session);
    assert_eq!(OwnerView::of(PaneOwner::Agent(mine), mine, live), OwnerView::You);
    assert_eq!(OwnerView::of(PaneOwner::Agent(theirs), mine, live), OwnerView::OtherAgent);
    assert_eq!(OwnerView::of(PaneOwner::User, mine, live), OwnerView::User);
    sessions.close(theirs);
    let live = |session| sessions.contains(session);
    assert_eq!(OwnerView::of(PaneOwner::Agent(theirs), mine, live), OwnerView::Orphaned);
    let spellings: Vec<_> =
        [OwnerView::User, OwnerView::You, OwnerView::OtherAgent, OwnerView::Orphaned]
            .iter()
            .map(|view| view.as_str())
            .collect();
    assert_eq!(spellings, ["user", "you", "other_agent", "orphaned"]);
}

/// One message per session per interval: a second inside it is refused and records nothing, so
/// the next admission is measured from the first; another session has its own clock; a closed or
/// unknown session is refused rather than admitted.
#[test]
fn a_session_admits_one_agent_message_per_interval() {
    let sessions = LiveSessions::default();
    let (mine, theirs) = (sessions.open(), sessions.open());
    let start = std::time::Instant::now();
    assert_eq!(sessions.admit_message(mine, start), MessageAdmission::Admitted);
    let early = start + AGENT_MESSAGE_INTERVAL / 2;
    assert_eq!(sessions.admit_message(mine, early), MessageAdmission::TooSoon);
    assert_eq!(sessions.admit_message(theirs, early), MessageAdmission::Admitted);
    let later = start + AGENT_MESSAGE_INTERVAL;
    assert_eq!(sessions.admit_message(mine, later), MessageAdmission::Admitted);
    sessions.close(mine);
    assert_eq!(sessions.admit_message(mine, later), MessageAdmission::Closed);
    assert_eq!(sessions.admit_message(AgentSessionId::next(), later), MessageAdmission::Closed);
}

/// The snapshot lists every live session newest first with its client name and activity; a tool
/// call counts and stamps the session, and only the first one hands back the name to announce.
#[test]
fn the_snapshot_lists_sessions_newest_first_with_their_activity() {
    let sessions = LiveSessions::default();
    let older = sessions.open();
    let newer = sessions.open();
    sessions.set_client(older, Some("claude".into()));
    let now = std::time::Instant::now();
    assert_eq!(sessions.record_call(older, now), Some("claude".to_owned()), "first call");
    assert_eq!(sessions.record_call(older, now), None, "announced once");
    assert_eq!(sessions.record_call(newer, now), Some("unknown".to_owned()), "no name given");
    assert_eq!(sessions.record_call(AgentSessionId::next(), now), None, "unknown session");
    let listed = sessions.snapshot();
    assert_eq!(listed.iter().map(|entry| entry.session).collect::<Vec<_>>(), [newer, older]);
    assert_eq!(listed[1].client.as_deref(), Some("claude"));
    assert_eq!(listed[1].tool_calls, 2);
    assert_eq!(listed[1].last_call, Some(now));
    assert_eq!(listed[0].client, None);
    assert!(!listed[1].announced, "announced only once the notice was admitted");
    sessions.mark_announced(older);
    assert!(sessions.snapshot()[1].announced);
}

/// Closing a session hands back what its departure needs: the client name, whether it was
/// announced, and whether the user disconnected it from the Agents page. An unknown or already
/// closed session hands back nothing.
#[test]
fn closing_hands_back_the_departure() {
    let sessions = LiveSessions::default();
    let session = sessions.open();
    sessions.set_client(session, Some("copilot".into()));
    sessions.mark_announced(session);
    let flag = sessions.take_for_disconnect(session).expect("live");
    assert!(!flag.load(std::sync::atomic::Ordering::Acquire), "the caller closes it");
    let departure = sessions.close(session).expect("was live");
    assert_eq!(departure.client.as_deref(), Some("copilot"));
    assert!(departure.announced);
    assert!(departure.user_disconnected);
    assert!(sessions.close(session).is_none());
    assert!(sessions.take_for_disconnect(session).is_none());
}

/// Disconnecting from the Agents page closes exactly the chosen connection: its close flag is set
/// and the session stays listed until its connection tears down; another session is untouched,
/// a session that already left is a no-op, and "all" closes every session still open.
#[test]
fn disconnecting_closes_only_the_chosen_connection() {
    let shared = super::super::McpShared::new(McpControl::Agent, None);
    let (first, second, third) =
        (shared.sessions.open(), shared.sessions.open(), shared.sessions.open());
    assert!(shared.disconnect_agent(first));
    assert_eq!(shared.sessions.is_closed(first), Some(true));
    assert_eq!(shared.sessions.is_closed(second), Some(false));
    assert!(shared.sessions.contains(first), "listed until the connection tears down");
    shared.sessions.close(third);
    assert!(!shared.disconnect_agent(third), "a session that already left");
    assert_eq!(shared.disconnect_all_agents(), 1, "only the session still open");
    assert_eq!(shared.sessions.is_closed(second), Some(true));
    let departure = shared.sessions.close(first).expect("still recorded");
    assert!(departure.user_disconnected);
}
