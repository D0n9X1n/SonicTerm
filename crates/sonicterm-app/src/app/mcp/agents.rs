//! Agent sessions and pane ownership: which connection created a pane, which sessions are still
//! connected, and whether a session's `"agent"` scope reaches a pane under `[mcp] isolation`.
//!
//! A session is one connection. Its id comes from a process-wide counter that a server restart
//! never resets, so a pane's owner never names a later session. A pane outlives its session: once
//! the session ends the pane is orphaned, which is derived from the live set rather than stored,
//! so a disconnect walks no panes and a create racing a disconnect is orphaned by construction.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use sonicterm_cfg::config::{McpControl, McpCreateAnchor, McpIsolation, McpRead};

/// One agent connection's identity, assigned when the connection starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AgentSessionId(u64);

/// The next session id; starts at 1 and only grows, across server restarts too.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

impl AgentSessionId {
    /// A fresh id no earlier session in this process had.
    // Ordering: NEXT_SESSION Relaxed; the counter only has to hand out distinct values.
    pub(crate) fn next() -> Self {
        Self(NEXT_SESSION.fetch_add(1, Ordering::Relaxed))
    }

    /// The id as a number, for logs.
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

/// Who created a pane. It lives on the pane, so it survives tab transfer and tear-out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PaneOwner {
    /// The user opened it.
    #[default]
    User,
    /// An agent session created it through `create_pane`; the session may since have ended.
    Agent(AgentSessionId),
}

impl PaneOwner {
    /// Whether an agent created the pane, whichever session and whether or not it is connected.
    pub(crate) fn is_agent(self) -> bool {
        matches!(self, Self::Agent(_))
    }

    /// A fresh agent session's pane when `agent`, else the user's, for test fixtures.
    #[cfg(test)]
    pub(crate) fn agent_if(agent: bool) -> Self {
        if agent {
            Self::Agent(AgentSessionId::next())
        } else {
            // When: not `agent`, the fixture pane is the user's.
            Self::User
        }
    }
}

/// How a pane's owner looks from one session, as `list_panes` and `read_pane` report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerView {
    /// The user opened it.
    User,
    /// The calling session created it.
    You,
    /// Another session that is still connected created it.
    OtherAgent,
    /// A session that has since ended created it.
    Orphaned,
}

impl OwnerView {
    /// How `owner` looks to `session`, with `live` saying whether a session is still connected.
    pub(crate) fn of(
        owner: PaneOwner,
        session: AgentSessionId,
        live: impl Fn(AgentSessionId) -> bool,
    ) -> Self {
        match owner {
            PaneOwner::User => Self::User,
            PaneOwner::Agent(creator) if creator == session => Self::You,
            PaneOwner::Agent(creator) if live(creator) => Self::OtherAgent,
            PaneOwner::Agent(_) => Self::Orphaned,
        }
    }

    /// The JSON spelling.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::You => "you",
            Self::OtherAgent => "other_agent",
            Self::Orphaned => "orphaned",
        }
    }
}

/// What one request may touch: the server's scopes when the loop ran it and the calling session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Access {
    /// `[mcp] control`.
    pub(crate) control: McpControl,
    /// `[mcp] isolation`.
    pub(crate) isolation: McpIsolation,
    /// `[mcp] create_anchor`.
    pub(crate) create_anchor: McpCreateAnchor,
    /// The session that sent the request.
    pub(crate) session: AgentSessionId,
}

impl Access {
    /// Whether this session's `"agent"` scope covers a pane `owner` created. Under `"shared"` it
    /// covers every agent pane, orphaned ones too; under `"session"` only the session's own, so a
    /// pane whose session ended is covered by no one until the user closes it.
    pub(crate) fn reaches(self, owner: PaneOwner) -> bool {
        match (owner, self.isolation) {
            (PaneOwner::User, _) => false,
            (PaneOwner::Agent(_), McpIsolation::Shared) => true,
            (PaneOwner::Agent(creator), McpIsolation::Session) => creator == self.session,
        }
    }

    /// Whether read tools under `read` may see a pane `owner` created: every pane under `"any"`,
    /// else the panes [`Self::reaches`] covers.
    pub(crate) fn reads(self, read: McpRead, owner: PaneOwner) -> bool {
        read == McpRead::Any || self.reaches(owner)
    }
}

#[cfg(test)]
impl From<McpControl> for Access {
    /// `control` for a fresh session under the defaults, `"shared"` and `create_anchor = "any"`,
    /// so a test about one agent needs no session bookkeeping.
    fn from(control: McpControl) -> Self {
        Self {
            control,
            isolation: McpIsolation::Shared,
            create_anchor: McpCreateAnchor::Any,
            session: AgentSessionId::next(),
        }
    }
}

/// The sessions still connected; a session leaves when its connection closes.
#[derive(Default)]
pub(crate) struct LiveSessions {
    /// The connected sessions; held only for one insert, remove or lookup.
    live: Mutex<HashSet<AgentSessionId>>,
}

impl LiveSessions {
    /// Start a session and return its fresh id.
    pub(crate) fn open(&self) -> AgentSessionId {
        let session = AgentSessionId::next();
        self.live.lock().insert(session);
        session
    }

    /// End `session`; its panes read as orphaned from now on.
    pub(crate) fn close(&self, session: AgentSessionId) {
        self.live.lock().remove(&session);
    }

    /// Whether `session` is still connected.
    pub(crate) fn contains(&self, session: AgentSessionId) -> bool {
        self.live.lock().contains(&session)
    }
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod agents_tests;
