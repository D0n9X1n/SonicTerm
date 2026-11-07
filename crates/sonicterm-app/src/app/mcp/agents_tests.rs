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
