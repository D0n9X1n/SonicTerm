//! Agent sessions and pane ownership: which connection created a pane, which sessions are still
//! connected, and whether a session's `"agent"` scope reaches a pane under `[mcp] isolation`.
//!
//! A session is one connection. Its id comes from a process-wide counter that a server restart
//! never resets, so a pane's owner never names a later session. A pane outlives its session: once
//! the session ends the pane is orphaned, which is derived from the live set rather than stored,
//! so a disconnect walks no panes and a create racing a disconnect is orphaned by construction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// The shortest gap between two messages one session puts in front of the user: a `notify`, or a
/// `focus_pane` that selects a tab and may raise the window.
pub(crate) const AGENT_MESSAGE_INTERVAL: Duration = Duration::from_secs(2);

/// Whether a session may put a message in front of the user now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageAdmission {
    /// Admitted; the session's clock restarts now.
    Admitted,
    /// The session's previous message was less than [`AGENT_MESSAGE_INTERVAL`] ago.
    TooSoon,
    /// The session is no longer connected, so it may show nothing.
    Closed,
}

/// One live session's record: who it says it is, what it has done, and how to close it.
struct SessionRecord {
    /// The name `initialize` reported, already sanitized; `None` until then.
    client: Option<String>,
    /// When the connection opened.
    connected_at: Instant,
    /// Tool calls admitted so far.
    tool_calls: u64,
    /// When the last tool call was admitted.
    last_call: Option<Instant>,
    /// When the session's last message was admitted, for [`AGENT_MESSAGE_INTERVAL`].
    last_message: Option<Instant>,
    /// Whether its connect notice was admitted to the notice queue.
    announced: bool,
    /// Whether the user disconnected it from the Agents page.
    user_disconnected: bool,
    /// The connection's close flag, set by [`super::close_connection`].
    closed: Arc<AtomicBool>,
}

/// A copy of one live session for the Agents page, taken under the lock and read without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSnapshot {
    /// The session.
    pub(crate) session: AgentSessionId,
    /// The client's sanitized, self-reported name, if it gave one.
    pub(crate) client: Option<String>,
    /// When the connection opened.
    pub(crate) connected_at: Instant,
    /// Tool calls admitted so far.
    pub(crate) tool_calls: u64,
    /// When the last tool call was admitted.
    pub(crate) last_call: Option<Instant>,
    /// Whether its connect notice was admitted.
    pub(crate) announced: bool,
}

/// What a closed session's departure needs, handed back by [`LiveSessions::close`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Departure {
    /// The client's sanitized, self-reported name, if it gave one.
    pub(crate) client: Option<String>,
    /// Whether its connect notice was admitted.
    pub(crate) announced: bool,
    /// Whether the user disconnected it from the Agents page, which shows its own confirmation.
    pub(crate) user_disconnected: bool,
}

/// The sessions still connected; a session leaves when its connection closes.
#[derive(Default)]
pub(crate) struct LiveSessions {
    /// The connected sessions' records; held only for one insert, remove, lookup, update or copy,
    /// and never while a connection is closed, since that takes the waiter lock.
    live: Mutex<HashMap<AgentSessionId, SessionRecord>>,
    /// Each session's resume token, kept past its close while it may own panes.
    pub(crate) tokens: super::claims::ResumeRegistry,
}

impl LiveSessions {
    /// Start a session with no call slot and its own close flag, and return its fresh id, for tests.
    #[cfg(test)]
    pub(crate) fn open(&self) -> AgentSessionId {
        self.open_connection(None, Arc::new(AtomicBool::new(false)))
    }

    /// Start a connection's session, whose calls set `in_flight` while they run and whose
    /// connection ends when `closed` is set, and mint its resume token.
    pub(crate) fn open_connection(
        &self,
        in_flight: Option<Arc<AtomicBool>>,
        closed: Arc<AtomicBool>,
    ) -> AgentSessionId {
        let session = AgentSessionId::next();
        let record = SessionRecord {
            client: None,
            connected_at: Instant::now(),
            tool_calls: 0,
            last_call: None,
            last_message: None,
            announced: false,
            user_disconnected: false,
            closed,
        };
        self.live.lock().insert(session, record);
        self.tokens.connect(session, in_flight);
        session
    }

    /// End `session`; its panes read as orphaned from now on, claimable with its token. Returns its
    /// departure, or `None` when it was not live.
    pub(crate) fn close(&self, session: AgentSessionId) -> Option<Departure> {
        let record = self.live.lock().remove(&session);
        self.tokens.disconnect(session);
        record.map(|record| Departure {
            client: record.client,
            announced: record.announced,
            user_disconnected: record.user_disconnected,
        })
    }

    /// Whether `session` is still connected.
    pub(crate) fn contains(&self, session: AgentSessionId) -> bool {
        self.live.lock().contains_key(&session)
    }

    /// Record the name `session`'s `initialize` reported.
    pub(crate) fn set_client(&self, session: AgentSessionId, client: Option<String>) {
        if let Some(record) = self.live.lock().get_mut(&session) {
            record.client = client;
        }
    }

    /// Count one admitted tool call of `session` at `now`. On its first call returns the name to
    /// announce (`unknown` when it gave none); `None` afterwards and for a session not live.
    pub(crate) fn record_call(&self, session: AgentSessionId, now: Instant) -> Option<String> {
        let mut live = self.live.lock();
        let record = live.get_mut(&session)?;
        record.tool_calls += 1;
        record.last_call = Some(now);
        (record.tool_calls == 1).then(|| record.client.clone().unwrap_or_else(|| "unknown".into()))
    }

    /// Remember that `session`'s connect notice was admitted.
    pub(crate) fn mark_announced(&self, session: AgentSessionId) {
        if let Some(record) = self.live.lock().get_mut(&session) {
            record.announced = true;
        }
    }

    /// Copy every live session, newest first; the lock is released before the copy is read.
    pub(crate) fn snapshot(&self) -> Vec<SessionSnapshot> {
        let mut listed: Vec<SessionSnapshot> = self
            .live
            .lock()
            .iter()
            .map(|(session, record)| SessionSnapshot {
                session: *session,
                client: record.client.clone(),
                connected_at: record.connected_at,
                tool_calls: record.tool_calls,
                last_call: record.last_call,
                announced: record.announced,
            })
            .collect();
        listed.sort_by_key(|entry| std::cmp::Reverse(entry.session.get()));
        listed
    }

    /// Mark `session` as disconnected by the user and hand back its close flag, for the caller to
    /// close outside this lock; `None` when the session already left.
    pub(crate) fn take_for_disconnect(&self, session: AgentSessionId) -> Option<Arc<AtomicBool>> {
        let mut live = self.live.lock();
        let record = live.get_mut(&session)?;
        record.user_disconnected = true;
        Some(Arc::clone(&record.closed))
    }

    /// [`Self::take_for_disconnect`] for every session whose connection is still open.
    // Ordering: closed Acquire pairs with close_connection's Release store.
    pub(crate) fn take_all_for_disconnect(&self) -> Vec<Arc<AtomicBool>> {
        let mut live = self.live.lock();
        live.values_mut()
            .filter(|record| !record.closed.load(Ordering::Acquire))
            .map(|record| {
                record.user_disconnected = true;
                Arc::clone(&record.closed)
            })
            .collect()
    }

    /// Whether `session`'s connection was closed, or `None` when it is not live, for tests.
    // Ordering: closed Acquire pairs with close_connection's Release store.
    #[cfg(test)]
    pub(crate) fn is_closed(&self, session: AgentSessionId) -> Option<bool> {
        self.live.lock().get(&session).map(|record| record.closed.load(Ordering::Acquire))
    }

    /// Admit one message from `session` at `now`: at most one per [`AGENT_MESSAGE_INTERVAL`]. The
    /// limit lives on the session, not on one call, so a client cannot reset it per call.
    pub(crate) fn admit_message(&self, session: AgentSessionId, now: Instant) -> MessageAdmission {
        let mut live = self.live.lock();
        let Some(record) = live.get_mut(&session) else {
            // When: `session` is not `live`, its connection closed and it shows nothing.
            return MessageAdmission::Closed;
        };
        let last = &mut record.last_message;
        if last.is_some_and(|last| now.saturating_duration_since(last) < AGENT_MESSAGE_INTERVAL) {
            // When: the `last` message is younger than the interval, this one is refused and the
            // clock keeps running from the last admitted one.
            return MessageAdmission::TooSoon;
        }
        *last = Some(now);
        MessageAdmission::Admitted
    }
}

impl super::McpShared {
    /// Disconnect `session` from the Agents page: close its connection as a client hang-up would,
    /// so its running wait ends `revoked` and its panes become claimable once it tears down.
    /// `false` when it had already left.
    // The sessions lock is released before close_connection takes the waiter lock; never both.
    pub(crate) fn disconnect_agent(&self, session: AgentSessionId) -> bool {
        let Some(closed) = self.sessions.take_for_disconnect(session) else {
            // When: `take_for_disconnect` finds no live `session`, it left already; nothing closes.
            return false;
        };
        super::close_connection(self, &closed);
        tracing::info!(target: "mcp", session = session.get(), "agent disconnected by the user");
        true
    }

    /// Disconnect every session whose connection is open now; returns how many were closed.
    // The sessions lock is released before close_connection takes the waiter lock; never both.
    pub(crate) fn disconnect_all_agents(&self) -> usize {
        let flags = self.sessions.take_all_for_disconnect();
        for closed in &flags {
            super::close_connection(self, closed);
        }
        tracing::info!(target: "mcp", count = flags.len(), "every agent disconnected by the user");
        flags.len()
    }
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod agents_tests;

#[cfg(test)]
#[path = "agents_disconnect_tests.rs"]
mod agents_disconnect_tests;
