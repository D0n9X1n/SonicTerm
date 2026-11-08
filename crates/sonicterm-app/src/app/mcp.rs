//! The local MCP control server: a listener thread, a reader and a worker thread per connection,
//! and a request inbox the event loop drains.
//!
//! Only lookups, authorization and mutations of app state run on the event loop; a request
//! reaches it as an entry in [`McpShared::inbox`] and a payload-free [`UserEvent::McpRequest`]
//! wake-up. Reads, regex and serialization run on the connection's worker thread. Every request
//! carries the connection's `closed` flag and the generation that admitted it, and the loop
//! rechecks both, the deadline and `enabled` before it acts, so a disable, a downgrade or a
//! disconnect drops queued work.

use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use sonicterm_cfg::config::{McpControl, McpCreateAnchor, McpIsolation, McpRead};
use sonicterm_vt::vt::Parser;
use winit::event_loop::EventLoopProxy;

use super::UserEvent;

#[path = "mcp/agents.rs"]
pub(crate) mod agents;
#[path = "mcp/claims.rs"]
pub(crate) mod claims;
#[path = "mcp/control_loop.rs"]
pub(crate) mod control_loop;
#[path = "mcp/dispatch.rs"]
pub(crate) mod dispatch;
#[cfg(unix)]
#[path = "mcp/endpoint_unix.rs"]
pub(crate) mod endpoint_unix;
#[cfg(windows)]
#[path = "mcp/endpoint_windows.rs"]
pub(crate) mod endpoint_windows;
#[path = "mcp/jsonrpc.rs"]
pub(crate) mod jsonrpc;
#[path = "mcp/keys.rs"]
pub(crate) mod keys;
#[path = "mcp/lifecycle.rs"]
mod lifecycle;
#[path = "mcp/read.rs"]
pub(crate) mod read;
#[path = "mcp/run.rs"]
pub(crate) mod run;
#[path = "mcp/scan.rs"]
pub(crate) mod scan;
#[path = "mcp/stream_scan.rs"]
pub(crate) mod stream_scan;
#[path = "mcp/stream_wait.rs"]
pub(crate) mod stream_wait;
#[path = "mcp/tools_control.rs"]
pub(crate) mod tools_control;
#[path = "mcp/tools_read.rs"]
pub(crate) mod tools_read;

pub(crate) use lifecycle::McpServer;

/// The default limit on connections served at once (`[mcp] max_connections`).
pub(crate) const MAX_CONNECTIONS: usize = sonicterm_cfg::config::DEFAULT_MCP_CONNECTIONS as usize;
/// How long a loop request may take before it is dropped.
pub(crate) const LOOP_DEADLINE: Duration = Duration::from_secs(10);
/// How long a reader blocks before it rechecks `closed` and stop flags.
pub(crate) const READ_POLL: Duration = Duration::from_millis(500);
/// How long a partial line may sit idle before the connection closes.
pub(crate) const PARTIAL_IDLE: Duration = Duration::from_secs(30);
/// The most connection notices waiting for the loop; more are dropped, never queued.
pub(crate) const MAX_PENDING_NOTICES: u64 = 8;
/// How long a reply write may block on a client that stopped reading.
pub(crate) const WRITE_DEADLINE: Duration = Duration::from_secs(5);

/// One accepted connection's byte streams.
pub(crate) struct Transport {
    /// Bytes from the client.
    pub(crate) reader: Box<dyn Read + Send>,
    /// Bytes to the client; its write timeout is already [`WRITE_DEADLINE`].
    pub(crate) writer: Box<dyn Write + Send>,
}

/// Work only the event loop may do, with the reply it owes.
pub(crate) enum LoopCall {
    /// Every pane in every window, with the handles a read needs.
    ListPanes,
    /// One pane's read handles, if it exists.
    ResolvePane {
        /// The pane to resolve.
        pane_id: u64,
    },
    /// Show `text` in the existing in-app notification; nobody waits for the reply.
    Notice {
        /// The sanitized, bounded message.
        text: String,
    },
    /// Open a helper pane beside `anchor`.
    CreatePane {
        /// The pane the helper opens beside.
        anchor: u64,
        /// A split or a new tab.
        placement: crate::app::pane_target::Placement,
        /// What the helper runs.
        launch: crate::app::pane_target::AgentLaunch,
    },
    /// Type `text` into a pane, then Enter when `submit` is set.
    SendText {
        /// The target pane.
        pane_id: u64,
        /// The text, at most 64 KiB.
        text: String,
        /// Whether to add `\r`.
        submit: bool,
    },
    /// Send parsed keys to a pane.
    SendKeys {
        /// The target pane.
        pane_id: u64,
        /// The keys, already parsed.
        keys: Vec<keys::KeySpec>,
    },
    /// Start a command at a pane's shell prompt.
    RunStart {
        /// The target pane.
        pane_id: u64,
        /// The command line, without control characters.
        command: String,
    },
    /// Close one pane.
    ClosePane {
        /// The pane to close.
        pane_id: u64,
    },
    /// Move a closed session's panes to the caller.
    ClaimPanes {
        /// The closed session's token.
        resume: claims::ResumeToken,
        /// The caller's own token, proving it received it.
        own: claims::ResumeToken,
    },
}

/// Where a pane sits in the UI, which only `list_panes` reports. Computing it orders every
/// window, so a tool that names one pane never asks for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneMetadata {
    /// The window's position: 0 for the main window, then children in creation order.
    pub(crate) window_index: usize,
    /// The pane's tab index in its window.
    pub(crate) tab_index: usize,
    /// Whether the pane is its window's active pane in the active tab.
    pub(crate) active: bool,
    /// Whether the pane's window is in READONLY.
    pub(crate) readonly: bool,
}

/// A pane the loop resolved: the handles a connection thread reads without the loop, plus its UI
/// position when it came from `list_panes`.
#[derive(Clone)]
pub(crate) struct PaneEntry {
    /// The pane's id.
    pub(crate) pane_id: u64,
    /// Where the pane sits; `Some` only for `list_panes` entries.
    pub(crate) metadata: Option<PaneMetadata>,
    /// Who created the pane; `agent_owned` in tool results is `creator.is_agent()`.
    pub(crate) creator: agents::PaneOwner,
    /// The pane's parser, read with one bounded attempt.
    pub(crate) parser: Arc<Mutex<Parser>>,
    /// Bumped after each batch of output; read without any lock.
    pub(crate) output_generation: Arc<AtomicU64>,
    /// The process's exit status once known.
    pub(crate) exit_status: Arc<OnceLock<sonicterm_types::PaneExitStatus>>,
    /// Announced after each output batch and after the exit is recorded; a wait sleeps on it.
    pub(crate) change_signal: Arc<crate::app::PaneSignal>,
    /// The first typing stamp since an agent run reset it; 0 when none.
    pub(crate) run_typed_at: Arc<AtomicU64>,
    /// Set while an agent run owns the pane.
    pub(crate) agent_run: Arc<AtomicBool>,
}

/// What the loop answered.
pub(crate) enum LoopReply {
    /// The panes [`LoopCall::ListPanes`] found.
    Panes(Vec<PaneEntry>),
    /// The pane [`LoopCall::ResolvePane`] found.
    Pane(PaneEntry),
    /// The request was refused, with a stable code and a message.
    Refused(&'static str, String),
    /// [`LoopCall::CreatePane`] opened this pane.
    Created(crate::app::pane_target::CreatedPane),
    /// A write queued this many bytes; `receipt` is its delivery ticket and the pane's probe, or
    /// `None` for a pane without a PTY.
    Written(usize, Option<(u64, sonicterm_io::pty::PtyDeliveryProbe)>),
    /// [`LoopCall::RunStart`] started a run.
    RunStarted(Box<run::RunStarted>),
    /// [`LoopCall::ClosePane`] closed the pane.
    Closed,
    /// [`LoopCall::ClaimPanes`] moved this many panes.
    Claimed(usize),
}

/// A request waiting in the inbox.
pub(crate) struct LoopRequest {
    /// The work asked for.
    pub(crate) call: LoopCall,
    /// The generation that admitted the request.
    pub(crate) generation: u64,
    /// When the request stops being worth answering.
    pub(crate) deadline: Instant,
    /// Set when the client is gone.
    pub(crate) closed: Arc<AtomicBool>,
    /// The agent session that asked, whose `"agent"` scopes the loop applies.
    pub(crate) session: agents::AgentSessionId,
    /// Where the answer goes; the loop only `try_send`s.
    pub(crate) reply: SyncSender<LoopReply>,
}

/// State shared by the listener, every connection and the event loop.
pub(crate) struct McpShared {
    /// Requests waiting for the event loop.
    pub(crate) inbox: Mutex<Vec<LoopRequest>>,
    /// Bumped when an `[mcp]` value changes or the server stops; older requests are dropped.
    pub(crate) generation: AtomicU64,
    /// The control level, as [`McpControl`]'s discriminant.
    pub(crate) control: AtomicU8,
    /// Whether read tools are limited to agent-created panes (`[mcp] read = "agent"`).
    pub(crate) read_agent_only: AtomicBool,
    /// `[mcp] isolation` and `create_anchor`, packed by [`agent_scope_code`].
    agent_scope: AtomicU8,
    /// Agent sessions still connected; a pane whose creator left reads as orphaned.
    pub(crate) sessions: agents::LiveSessions,
    /// Set to stop the listener and every connection.
    pub(crate) stopped: AtomicBool,
    /// Whether a wake-up is already queued, so a burst of requests wakes the loop once.
    wake_pending: AtomicBool,
    /// Wakes the event loop; `None` in tests, where the inbox is drained directly.
    proxy: Mutex<Option<EventLoopProxy<UserEvent>>>,
    /// Live connections, at most `connection_limit`.
    pub(crate) connections: AtomicU64,
    /// The most connections served at once; a further one is refused. A change applies to new
    /// connections only.
    pub(crate) connection_limit: AtomicU64,
    /// Connection notices queued and not yet shown, at most [`MAX_PENDING_NOTICES`].
    pending_notices: AtomicU64,
    /// Bytes every connection holds right now: partial request lines and replies being written.
    /// The loop charges this figure to the governor's `Mcp` owner.
    pub(crate) buffered_bytes: AtomicU64,
    /// The change signals of panes a wait is sleeping on, so a revocation wakes the wait at once
    /// rather than at its next recheck. Entries are weak: a closed pane's signal is not kept.
    waiters: Mutex<Vec<Weak<crate::app::PaneSignal>>>,
    /// A test's replacement for `wait_for`'s recheck interval, so an end-to-end test can prove a
    /// wake came from a signal and not from the recheck.
    #[cfg(test)]
    pub(crate) recheck_override: Mutex<Option<Duration>>,
}

/// A wait's registration in [`McpShared`]'s waiter list; dropping it removes the entry.
pub(crate) struct WaiterGuard<'shared> {
    /// Where the entry lives.
    shared: &'shared McpShared,
    /// The registered signal, compared by address on removal.
    signal: Weak<crate::app::PaneSignal>,
}

// Lifecycle: WaiterGuard removes its signal from the waiter list when the wait ends, however it ends.
impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        let mut waiters = self.shared.waiters.lock();
        // Only this registration goes: another wait on the same pane keeps its own entry.
        if let Some(index) = waiters.iter().position(|entry| entry.ptr_eq(&self.signal)) {
            waiters.swap_remove(index);
        }
        waiters.retain(|entry| entry.strong_count() > 0);
    }
}

impl McpShared {
    /// Shared state at `control`, waking the loop through `proxy`.
    pub(crate) fn new(control: McpControl, proxy: Option<EventLoopProxy<UserEvent>>) -> Arc<Self> {
        Arc::new(Self {
            inbox: Mutex::new(Vec::new()),
            generation: AtomicU64::new(1),
            control: AtomicU8::new(control_code(control)),
            read_agent_only: AtomicBool::new(false),
            agent_scope: AtomicU8::new(agent_scope_code(
                McpIsolation::Shared,
                McpCreateAnchor::Any,
            )),
            sessions: agents::LiveSessions::default(),
            stopped: AtomicBool::new(false),
            wake_pending: AtomicBool::new(false),
            proxy: Mutex::new(proxy),
            connections: AtomicU64::new(0),
            connection_limit: AtomicU64::new(MAX_CONNECTIONS as u64),
            pending_notices: AtomicU64::new(0),
            buffered_bytes: AtomicU64::new(0),
            waiters: Mutex::new(Vec::new()),
            #[cfg(test)]
            recheck_override: Mutex::new(None),
        })
    }

    /// Register a wait sleeping on `signal` until the returned guard drops, so a revocation wakes it.
    pub(crate) fn register_waiter(&self, signal: &Arc<crate::app::PaneSignal>) -> WaiterGuard<'_> {
        let signal = Arc::downgrade(signal);
        self.waiters.lock().push(signal.clone());
        WaiterGuard { shared: self, signal }
    }

    /// How many waits are registered right now, so a test knows a wait reached its sleep loop.
    #[cfg(test)]
    pub(crate) fn waiter_count(&self) -> usize {
        self.waiters.lock().len()
    }

    /// Wake every registered wait so it rechecks its revocation now.
    pub(crate) fn wake_waiters(&self) {
        let signals: Vec<_> = self.waiters.lock().iter().filter_map(Weak::upgrade).collect();
        for signal in signals {
            signal.notify();
        }
    }

    /// The current control level.
    // Ordering: control Acquire pairs with the Release store in `set_control`; the level is read alone.
    pub(crate) fn control(&self) -> McpControl {
        control_from_code(self.control.load(Ordering::Acquire))
    }

    /// The current read scope.
    // Ordering: read_agent_only Acquire pairs with the Release store in `set_read`; read alone.
    pub(crate) fn read(&self) -> McpRead {
        // When: `read_agent_only` is set the scope is `Agent`, else `Any`.
        if self.read_agent_only.load(Ordering::Acquire) {
            McpRead::Agent
        } else {
            McpRead::Any
        }
    }

    /// Change the read scope and revoke every queued request and running wait.
    // Ordering: read_agent_only Release publishes the scope before the generation AcqRel bump.
    pub(crate) fn set_read(&self, read: McpRead) {
        self.read_agent_only.store(read == McpRead::Agent, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.sessions.tokens.reissue();
        self.wake_waiters();
    }

    /// The current `[mcp] isolation` and `create_anchor`.
    // Ordering: agent_scope Acquire pairs with the Release store in `set_agent_scope`.
    pub(crate) fn agent_scope(&self) -> (McpIsolation, McpCreateAnchor) {
        let code = self.agent_scope.load(Ordering::Acquire);
        let isolation = if code & 1 == 0 { McpIsolation::Shared } else { McpIsolation::Session };
        let anchor = if code & 2 == 0 { McpCreateAnchor::Any } else { McpCreateAnchor::Agent };
        (isolation, anchor)
    }

    /// Change `isolation` and `create_anchor` and revoke every queued request and running wait,
    /// as a `control` or `read` change does: both narrow or widen what a call may touch.
    // Ordering: agent_scope Release publishes the scope before the generation AcqRel bump.
    pub(crate) fn set_agent_scope(&self, isolation: McpIsolation, anchor: McpCreateAnchor) {
        self.agent_scope.store(agent_scope_code(isolation, anchor), Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.sessions.tokens.reissue();
        self.wake_waiters();
    }

    /// What `session` may touch right now.
    pub(crate) fn access(&self, session: agents::AgentSessionId) -> agents::Access {
        let (isolation, create_anchor) = self.agent_scope();
        agents::Access { control: self.control(), isolation, create_anchor, session }
    }

    /// Set the connection limit for connections that arrive from now on; live ones are untouched.
    // Ordering: connection_limit Release pairs with spawn_connection's Acquire load.
    pub(crate) fn set_connection_limit(&self, limit: usize) {
        self.connection_limit.store(limit as u64, Ordering::Release);
    }

    /// Change the control level and revoke every queued request.
    // Ordering: control Release publishes the level before the generation AcqRel bump revokes requests.
    pub(crate) fn set_control(&self, control: McpControl) {
        self.control.store(control_code(control), Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.sessions.tokens.reissue();
        self.wake_waiters();
    }

    /// Stop the listener and every connection, and revoke every queued request.
    // Ordering: stopped Release publishes the stop before the generation AcqRel bump; readers Acquire both.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.sessions.tokens.reissue();
        self.wake_waiters();
    }

    /// Whether the server was stopped.
    // Ordering: stopped Acquire pairs with the Release store in `stop`.
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Ask the event loop to do `call` for the tool call `scope`, waiting at most until
    /// `deadline`. The request carries the generation that admitted the call, so a disable or a
    /// control change since then drops it unrun.
    pub(crate) fn ask(&self, call: LoopCall, scope: &CallScope) -> LoopReply {
        let closed = &scope.closed;
        if scope.revoked(self) {
            // When: the call `scope` was already `revoked`, nothing is queued.
            return LoopReply::Refused("revoked", "the request was dropped before it ran".into());
        }
        let deadline = Instant::now() + LOOP_DEADLINE;
        let (reply, answer) = sync_channel(1);
        let request = LoopRequest {
            call,
            generation: scope.generation,
            deadline,
            closed: Arc::clone(closed),
            session: scope.session,
            reply,
        };
        self.inbox.lock().push(request);
        if !self.wake() {
            // When: `wake` could not reach the event loop, nothing will answer the request.
            return LoopReply::Refused("unavailable", "SonicTerm's event loop is gone".into());
        }
        wait_reply(&answer, deadline, closed)
    }

    /// Wake the event loop once for a burst of requests; `false` when the loop is gone.
    // Ordering: wake_pending AcqRel makes the swap the single point that decides who sends.
    pub(crate) fn wake(&self) -> bool {
        if self.wake_pending.swap(true, Ordering::AcqRel) {
            // When: `wake_pending` was already set, a queued wake-up will drain this request too.
            return true;
        }
        let proxy = self.proxy.lock();
        match proxy.as_ref() {
            Some(proxy) => proxy.send_event(UserEvent::McpRequest).is_ok(),
            // Without a proxy (tests), the test drains the inbox itself.
            None => true,
        }
    }

    /// Count `bytes` as held by a connection until a matching [`Self::release_bytes`].
    // Ordering: buffered_bytes Relaxed; a sampled gauge that orders no other memory.
    pub(crate) fn hold_bytes(&self, bytes: usize) {
        self.buffered_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Stop counting `bytes` a connection held.
    // Ordering: buffered_bytes Relaxed; a sampled gauge that orders no other memory.
    pub(crate) fn release_bytes(&self, bytes: usize) {
        self.buffered_bytes.fetch_sub(bytes as u64, Ordering::Relaxed);
    }

    /// Bytes every connection holds right now.
    // Ordering: buffered_bytes Relaxed; a sampled gauge.
    pub(crate) fn buffered(&self) -> u64 {
        self.buffered_bytes.load(Ordering::Relaxed)
    }

    /// Show `text` in the in-app notification without waiting: the request is queued and the
    /// loop shows it when it next drains. A stopped server shows nothing.
    // Ordering: generation Acquire reads the value the loop compares; pending_notices AcqRel makes
    // the increment the single admission point for a notice.
    pub(crate) fn notify(&self, text: String) {
        if self.is_stopped() {
            // When: the server `is_stopped`, its connections end without a notice.
            return;
        }
        if self.pending_notices.fetch_add(1, Ordering::AcqRel) >= MAX_PENDING_NOTICES {
            // When: `MAX_PENDING_NOTICES` are already waiting, connection churn cannot grow the inbox.
            self.pending_notices.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        // The reply channel is dropped at once; the loop's `try_send` to it is ignored.
        let (reply, _answer) = sync_channel(1);
        self.inbox.lock().push(LoopRequest {
            call: LoopCall::Notice { text },
            generation: self.generation.load(Ordering::Acquire),
            deadline: Instant::now() + LOOP_DEADLINE,
            closed: Arc::new(AtomicBool::new(false)),
            // A notice reads no pane, so the session it carries is never checked.
            session: agents::AgentSessionId::next(),
            reply,
        });
        let _ = self.wake();
    }

    /// Mark one queued notice as shown.
    // Ordering: pending_notices AcqRel pairs with the admitting increment in `notify`.
    pub(crate) fn notice_shown(&self) {
        self.pending_notices.fetch_sub(1, Ordering::AcqRel);
    }

    /// Take every queued request; the loop calls this when woken. The wake flag stays set, so a
    /// request that arrives while the loop answers posts no second wake-up into the same pass;
    /// `about_to_wait` re-arms it ([`Self::wake_for_backlog`]).
    pub(crate) fn take_requests(&self) -> Vec<LoopRequest> {
        std::mem::take(&mut *self.inbox.lock())
    }
}

/// Wait for the loop's reply until `deadline`, giving up early when the client goes away.
// Ordering: closed Acquire pairs with the reader's Release store when it sees EOF.
fn wait_reply(answer: &Receiver<LoopReply>, deadline: Instant, closed: &AtomicBool) -> LoopReply {
    loop {
        let now = Instant::now();
        if now >= deadline || closed.load(Ordering::Acquire) {
            // When: the `deadline` passed or the client is `closed`, the answer is no longer wanted.
            return LoopReply::Refused("revoked", "the request was dropped before it ran".into());
        }
        let wait = (deadline - now).min(Duration::from_millis(100));
        match answer.recv_timeout(wait) {
            Ok(reply) => {
                // When: the loop sent a `reply`, it is the answer.
                return reply;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // When: `Timeout` elapsed, the deadline and `closed` are rechecked.
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // When: the loop dropped the reply sender (`Disconnected`), no answer will come.
                return LoopReply::Refused(
                    "revoked",
                    "the request was dropped before it ran".into(),
                );
            }
        }
    }
}

/// Encode a control level for the atomic.
fn control_code(control: McpControl) -> u8 {
    match control {
        McpControl::Off => 0,
        McpControl::Agent => 1,
        McpControl::Any => 2,
    }
}

/// Decode a control level from the atomic; an unknown code is the safest level.
fn control_from_code(code: u8) -> McpControl {
    match code {
        1 => McpControl::Agent,
        2 => McpControl::Any,
        _ => McpControl::Off,
    }
}

/// One tool call's admission: the connection's `closed` flag and the server generation that
/// accepted the call. Every loop request and every result the call returns is checked against
/// it, so a disable, a control change or a disconnect after acceptance revokes the whole call.
pub(crate) struct CallScope {
    /// Set when the client is gone.
    pub(crate) closed: Arc<AtomicBool>,
    /// The generation that accepted the call.
    pub(crate) generation: u64,
    /// The connection's agent session.
    pub(crate) session: agents::AgentSessionId,
}

impl CallScope {
    /// Admit a call now, on `shared`, for `session`'s connection whose close flag is `closed`.
    // Ordering: generation Acquire reads the value `set_control` and `stop` publish with AcqRel.
    pub(crate) fn admit_for(
        shared: &McpShared,
        closed: &Arc<AtomicBool>,
        session: agents::AgentSessionId,
    ) -> Self {
        Self {
            closed: Arc::clone(closed),
            generation: shared.generation.load(Ordering::Acquire),
            session,
        }
    }

    /// [`Self::admit_for`] a fresh live session, for tests that drive one connection's calls.
    #[cfg(test)]
    pub(crate) fn admit(shared: &McpShared, closed: &Arc<AtomicBool>) -> Self {
        Self::admit_for(shared, closed, shared.sessions.open())
    }

    /// Whether the call was revoked since it was admitted.
    // Ordering: closed and generation Acquire pair with their writers' Release stores.
    pub(crate) fn revoked(&self, shared: &McpShared) -> bool {
        self.closed.load(Ordering::Acquire)
            || shared.is_stopped()
            || shared.generation.load(Ordering::Acquire) != self.generation
    }
}

/// One connection's view of who called: the pane the bridge reported and the client's name.
#[derive(Debug, Clone, Default)]
pub(crate) struct Caller {
    /// The pane the bridge said the agent runs in; a default only, never authorization.
    pub(crate) self_pane: Option<u64>,
    /// The name `initialize` reported, for display and logs only.
    pub(crate) client: Option<String>,
    /// Whether `initialize` already ran; only the first one is announced.
    pub(crate) initialized: bool,
}

/// `isolation` and `create_anchor` as one byte: bit 0 is `"session"`, bit 1 is `"agent"`.
fn agent_scope_code(isolation: McpIsolation, anchor: McpCreateAnchor) -> u8 {
    u8::from(isolation == McpIsolation::Session) | (u8::from(anchor == McpCreateAnchor::Agent) << 1)
}

/// Serve one connection until it closes or the server stops: a reader thread parses lines and
/// answers what needs no app state, and a worker thread runs tool calls one at a time. Returns the
/// name the client gave in `initialize`, if it did.
pub(crate) fn serve_connection(shared: Arc<McpShared>, transport: Transport) -> Option<String> {
    let closed = Arc::new(AtomicBool::new(false));
    let writer = Arc::new(Mutex::new(transport.writer));
    let caller = Arc::new(Mutex::new(Caller::default()));
    let (calls, call_queue) = sync_channel::<AdmittedCall>(1);
    let in_flight = Arc::new(AtomicBool::new(false));
    let worker = {
        let (shared, writer, caller, closed, in_flight) = (
            Arc::clone(&shared),
            Arc::clone(&writer),
            Arc::clone(&caller),
            Arc::clone(&closed),
            Arc::clone(&in_flight),
        );
        std::thread::Builder::new().name("sonicterm-mcp-worker".into()).spawn(move || {
            run_calls(&shared, &call_queue, &writer, &caller, &closed, &in_flight);
        })
    };
    if worker.is_err() {
        // When: the worker thread could not start, the connection cannot run tools and closes.
        return None;
    }
    // The session starts before the first call is admitted and ends with the connection, so a
    // pane it created reads as orphaned from the moment the client is gone.
    let session = shared.sessions.open_connection(Some(Arc::clone(&in_flight)));
    let calls = CallSlot { queue: calls, in_flight, closed: Arc::clone(&closed), session };
    read_loop(&shared, transport.reader, &writer, &caller, &calls, &closed);
    close_connection(&shared, &closed);
    shared.sessions.close(session);
    let client = caller.lock().client.clone();
    client
}

/// The worker: run admitted tool calls one at a time and write each reply.
// Ordering: in_flight Release frees the call slot before the reply is written, so a prompt next
// call finds it free.
fn run_calls(
    shared: &McpShared,
    call_queue: &Receiver<AdmittedCall>,
    writer: &Mutex<Box<dyn Write + Send>>,
    caller: &Mutex<Caller>,
    closed: &Arc<AtomicBool>,
    in_flight: &AtomicBool,
) {
    for call in call_queue {
        let snapshot = caller.lock().clone();
        let run = |params: &Value| tools_read::call(shared, &snapshot, params, &call.scope);
        let respond = |result| jsonrpc::response(&call.id, result);
        let reply = dispatch::run_logged(&call.params, call.wire_bytes, &snapshot, run, respond);
        // The call's arguments stop being charged once its result is formed.
        drop(call);
        // The slot frees before the client can see the reply; freeing it after the write let a
        // client's prompt next call find it still taken and be refused.
        in_flight.store(false, Ordering::Release);
        write_line(shared, writer, &reply, closed);
    }
}

/// Releases a held byte count when the bytes are no longer held, on every path.
struct HeldBytes<'a> {
    /// The server whose gauge holds the bytes.
    shared: &'a McpShared,
    /// How many bytes to release.
    bytes: usize,
}

// Lifecycle: HeldBytes drop calls release_bytes once the buffer is written or abandoned.
impl Drop for HeldBytes<'_> {
    fn drop(&mut self) {
        self.shared.release_bytes(self.bytes);
    }
}

/// A tool call the reader accepted: admitted under the generation current at acceptance, with its
/// arguments charged to the gauge from acceptance until the call is dropped after its reply.
pub(crate) struct AdmittedCall {
    /// The request id the reply carries.
    id: Value,
    /// The `tools/call` params.
    params: Value,
    /// The admission the whole call is checked against.
    scope: CallScope,
    /// The server whose gauge holds `bytes`.
    shared: Arc<McpShared>,
    /// The bytes charged for `params`.
    bytes: usize,
    /// The request line's length on the wire, newline excluded, for the call's log line.
    wire_bytes: usize,
}

impl AdmittedCall {
    /// Admit a call now: its scope takes the current generation and its arguments are charged.
    fn admit_on(
        shared: &Arc<McpShared>,
        calls: &CallSlot,
        request: (Value, Value),
        wire: usize,
    ) -> Self {
        let mut call =
            Self::admit_scoped(shared, &calls.closed, calls.session, request.0, request.1);
        call.wire_bytes = wire;
        call
    }

    /// [`Self::admit_on`] for a fresh live session, for tests without a connection's slot.
    #[cfg(test)]
    fn admit(shared: &Arc<McpShared>, closed: &Arc<AtomicBool>, id: Value, params: Value) -> Self {
        Self::admit_scoped(shared, closed, shared.sessions.open(), id, params)
    }

    /// Admit a call for `session` on the connection whose close flag is `closed`.
    fn admit_scoped(
        shared: &Arc<McpShared>,
        closed: &Arc<AtomicBool>,
        session: agents::AgentSessionId,
        id: Value,
        params: Value,
    ) -> Self {
        let bytes = params.to_string().len();
        shared.hold_bytes(bytes);
        let scope = CallScope::admit_for(shared, closed, session);
        Self { id, params, scope, shared: Arc::clone(shared), bytes, wire_bytes: 0 }
    }
}

// Lifecycle: AdmittedCall drop calls release_bytes for the arguments it charged.
impl Drop for AdmittedCall {
    fn drop(&mut self) {
        self.shared.release_bytes(self.bytes);
    }
}

/// The connection's one tool-call slot: a call is admitted only while none is running, so a
/// second call is refused rather than queued behind a long `wait_for`.
struct CallSlot {
    /// Admitted calls, for the worker.
    queue: SyncSender<AdmittedCall>,
    /// Set from admission until the worker has formed the result, just before it writes the reply.
    in_flight: Arc<AtomicBool>,
    /// The connection's close flag, which every admitted call carries.
    closed: Arc<AtomicBool>,
    /// The connection's agent session, which every admitted call carries.
    session: agents::AgentSessionId,
}

impl CallSlot {
    /// Claim the slot for a new call; `false` while another call runs.
    // Ordering: in_flight AcqRel makes the swap the single point that admits a call.
    fn claim(&self) -> bool {
        !self.in_flight.swap(true, Ordering::AcqRel)
    }
}

/// Read lines until the client closes, a frame is too long, a partial line idles too long, or
/// the server stops.
// Ordering: closed Acquire pairs with the Release stores in `write_line` and the caller.
fn read_loop(
    shared: &Arc<McpShared>,
    reader: Box<dyn Read + Send>,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    caller: &Arc<Mutex<Caller>>,
    calls: &CallSlot,
    closed: &Arc<AtomicBool>,
) {
    let mut reader = BufReader::new(reader);
    let mut partial = Vec::new();
    let mut idle_since = Instant::now();
    // The partial line's capacity is held for as long as the connection keeps it.
    let mut held = HeldBytes { shared, bytes: 0 };
    while !shared.is_stopped() && !closed.load(Ordering::Acquire) {
        let frame = jsonrpc::read_frame(&mut reader, &mut partial);
        // The gauge follows the partial line's capacity as it grows or is released.
        if partial.capacity() != held.bytes {
            shared.hold_bytes(partial.capacity());
            shared.release_bytes(held.bytes);
            held.bytes = partial.capacity();
        }
        let line = match frame {
            jsonrpc::Frame::Line(line) => line,
            jsonrpc::Frame::Idle if partial.is_empty() || idle_since.elapsed() < PARTIAL_IDLE => {
                // When: `Idle` with no `partial` line, or one younger than `PARTIAL_IDLE`, waits on.
                if partial.is_empty() {
                    idle_since = Instant::now();
                }
                continue;
            }
            jsonrpc::Frame::Idle | jsonrpc::Frame::Closed | jsonrpc::Frame::TooLong => {
                // When: the client is `Closed`, sent a `TooLong` line, or let a partial line go `Idle`.
                return;
            }
        };
        idle_since = Instant::now();
        handle_line(shared, &line, writer, caller, calls, closed);
    }
}

/// Answer one line: the preamble and the stateless methods here, tool calls on the worker.
fn handle_line(
    shared: &Arc<McpShared>,
    line: &str,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    caller: &Arc<Mutex<Caller>>,
    calls: &CallSlot,
    closed: &Arc<AtomicBool>,
) {
    let reply = match jsonrpc::parse_line(line) {
        jsonrpc::Incoming::Preamble { pane_id } => {
            // When: the bridge `Preamble` names the agent's `pane_id`, it is recorded unanswered.
            caller.lock().self_pane = pane_id;
            return;
        }
        jsonrpc::Incoming::Notification { .. } => {
            // When: a `Notification` expects no reply, nothing is written.
            return;
        }
        jsonrpc::Incoming::Invalid { id, code, message } => {
            jsonrpc::error_response(&id, code, message)
        }
        jsonrpc::Incoming::Request { id, method, params } => {
            // When: a `Request` is answered here unless its `method` hands a call to the worker.
            let Some(reply) =
                answer_request(shared, &id, &method, params, line.len(), caller, calls)
            else {
                // When: `answer_request` handed the call to the worker, which writes the `reply`.
                return;
            };
            reply
        }
    };
    write_line(shared, writer, &reply, closed);
}

/// The reply to one request, or `None` when the worker took a tool call and replies itself.
fn answer_request(
    shared: &Arc<McpShared>,
    id: &Value,
    method: &str,
    params: Value,
    wire_bytes: usize,
    caller: &Mutex<Caller>,
    calls: &CallSlot,
) -> Option<String> {
    let reply = match method {
        "initialize" => {
            let first = !std::mem::replace(&mut caller.lock().initialized, true);
            if first {
                // Only a connection's first `initialize` names the client and is announced.
                let client = jsonrpc::client_name(&params);
                let name = client.as_deref().unwrap_or("unknown");
                tracing::info!(target: "mcp", client = name, "agent connected");
                shared.notify(format!("Agent connected: {name}"));
                caller.lock().client = client;
            }
            jsonrpc::response(id, jsonrpc::initialize_result(&params))
        }
        "ping" => jsonrpc::response(id, serde_json::json!({})),
        "tools/list" => jsonrpc::response(id, tools_read::tool_list()),
        "tools/call" if !params.get("name").is_some_and(Value::is_string) => {
            jsonrpc::error_response(id, jsonrpc::INVALID_PARAMS, "tools/call needs a tool name")
        }
        "tools/call" if !calls.claim() => jsonrpc::error_response(
            id,
            jsonrpc::INVALID_REQUEST,
            "one tool call at a time per connection",
        ),
        "tools/call" => {
            // When: this `method` claimed the call slot, the worker replies; a failed send means it is
            // gone and the connection is ending.
            let call = AdmittedCall::admit_on(shared, calls, (id.clone(), params), wire_bytes);
            let _ = calls.queue.try_send(call);
            return None;
        }
        _ => jsonrpc::error_response(id, jsonrpc::METHOD_NOT_FOUND, "method not found"),
    };
    Some(reply)
}

/// Write one line to the client; a failed or timed-out write marks the connection closed.
fn write_line(
    shared: &McpShared,
    writer: &Mutex<Box<dyn Write + Send>>,
    line: &str,
    closed: &AtomicBool,
) {
    // The reply is held from here until it is written or abandoned.
    shared.hold_bytes(line.len());
    let _held = HeldBytes { shared, bytes: line.len() };
    let mut writer = writer.lock();
    let written = writer
        .write_all(line.as_bytes())
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush());
    // A failed or timed-out write means the client stopped reading; it is dropped.
    if written.is_err() {
        drop(writer);
        close_connection(shared, closed);
    }
}

/// Mark a connection closed and wake every sleeping wait, so this connection's running call sees
/// the close at once; the other waits find nothing changed and sleep again. Every path that ends a
/// connection goes through here.
// Ordering: closed Release publishes the close to the reader and every waiter that Acquires it.
pub(crate) fn close_connection(shared: &McpShared, closed: &AtomicBool) {
    closed.store(true, Ordering::Release);
    shared.wake_waiters();
}

/// Run `serve_connection` on a new thread, counting it against the server's `connection_limit`; a
/// connection past the limit is dropped at once.
// Ordering: connection_limit Acquire pairs with set_connection_limit's Release; connections AcqRel
// makes the admit decision and the release atomic.
pub(crate) fn spawn_connection(shared: &Arc<McpShared>, transport: Transport) {
    let limit = shared.connection_limit.load(Ordering::Acquire);
    if shared.connections.fetch_add(1, Ordering::AcqRel) >= limit {
        // When: `connections` was already at the `limit`, this one is refused.
        shared.connections.fetch_sub(1, Ordering::AcqRel);
        tracing::warn!(target: "mcp", "agent connection refused: too many connections");
        return;
    }
    let shared = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("sonicterm-mcp-conn".into()).spawn({
        let shared = Arc::clone(&shared);
        move || serve_and_release(shared, transport)
    });
    // A connection thread that could not start releases its slot at once.
    if spawned.is_err() {
        shared.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A connection thread's body: serve, then release the slot.
// Ordering: connections AcqRel pairs with the admitting fetch_add in `spawn_connection`.
fn serve_and_release(shared: Arc<McpShared>, transport: Transport) {
    let client = serve_connection(Arc::clone(&shared), transport);
    shared.connections.fetch_sub(1, Ordering::AcqRel);
    tracing::info!(target: "mcp", "agent disconnected");
    // Only a connection that introduced itself was announced, so only it is announced leaving.
    if let Some(name) = client {
        shared.notify(format!("Agent disconnected: {name}"));
    }
}

/// What keeps a bound endpoint's file in place; dropping it removes the file. A named pipe has
/// no file, so Windows holds nothing.
#[cfg(unix)]
pub(crate) type EndpointGuard = endpoint_unix::SocketGuard;
/// What keeps a bound endpoint's discovery record in place; dropping it removes the record.
#[cfg(windows)]
pub(crate) type EndpointGuard = Option<endpoint_windows::DiscoveryGuard>;

/// Open this platform's endpoint and start its listener thread; returns the endpoint's path or
/// pipe name, published to panes as `SONICTERM_CONTROL`, and its guard.
pub(crate) fn start_listener(shared: &Arc<McpShared>) -> std::io::Result<(String, EndpointGuard)> {
    #[cfg(unix)]
    {
        endpoint_unix::start(shared).map(|endpoint| (endpoint.path, endpoint.guard))
    }
    #[cfg(windows)]
    {
        endpoint_windows::start(shared)
    }
}

#[cfg(test)]
#[path = "mcp_test_support.rs"]
mod mcp_test_support;
#[cfg(all(test, unix))]
pub(crate) use mcp_test_support::read_reply_line;
#[cfg(test)]
pub(crate) use mcp_test_support::test_entry;

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;

#[cfg(test)]
#[path = "mcp_read_scope_tests.rs"]
mod mcp_read_scope_tests;

#[cfg(test)]
#[path = "mcp_isolation_tests.rs"]
mod mcp_isolation_tests;
