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
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use sonicterm_cfg::config::McpControl;
use sonicterm_vt::vt::Parser;
use winit::event_loop::EventLoopProxy;

use super::UserEvent;

#[cfg(unix)]
#[path = "mcp/endpoint_unix.rs"]
pub(crate) mod endpoint_unix;
#[cfg(windows)]
#[path = "mcp/endpoint_windows.rs"]
pub(crate) mod endpoint_windows;
#[path = "mcp/jsonrpc.rs"]
pub(crate) mod jsonrpc;
#[path = "mcp/lifecycle.rs"]
mod lifecycle;
#[path = "mcp/read.rs"]
pub(crate) mod read;
#[path = "mcp/tools_control.rs"]
pub(crate) mod tools_control;
#[path = "mcp/tools_read.rs"]
pub(crate) mod tools_read;

pub(crate) use lifecycle::McpServer;

/// The most connections served at once; a further one is refused.
pub(crate) const MAX_CONNECTIONS: usize = 4;
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
}

/// A pane the loop resolved, with handles a connection thread reads without the loop.
#[derive(Clone)]
pub(crate) struct PaneEntry {
    /// The pane's id.
    pub(crate) pane_id: u64,
    /// The window's position: 0 for the main window, then children in creation order.
    pub(crate) window_index: usize,
    /// The pane's tab index in its window.
    pub(crate) tab_index: usize,
    /// Whether the pane is its window's active pane in the active tab.
    pub(crate) active: bool,
    /// Whether the pane's window is in READONLY.
    pub(crate) readonly: bool,
    /// Whether an agent created the pane.
    pub(crate) agent_owned: bool,
    /// The pane's parser, read with one bounded attempt.
    pub(crate) parser: Arc<Mutex<Parser>>,
    /// Bumped after each batch of output; read without any lock.
    pub(crate) output_generation: Arc<AtomicU64>,
    /// The process's exit status once known.
    pub(crate) exit_status: Arc<OnceLock<sonicterm_types::PaneExitStatus>>,
}

/// What the loop answered.
pub(crate) enum LoopReply {
    /// The panes [`LoopCall::ListPanes`] found.
    Panes(Vec<PaneEntry>),
    /// The pane [`LoopCall::ResolvePane`] found.
    Pane(PaneEntry),
    /// The request was refused, with a stable code and a message.
    Refused(&'static str, String),
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
    /// Set to stop the listener and every connection.
    pub(crate) stopped: AtomicBool,
    /// Whether a wake-up is already queued, so a burst of requests wakes the loop once.
    wake_pending: AtomicBool,
    /// Wakes the event loop; `None` in tests, where the inbox is drained directly.
    proxy: Mutex<Option<EventLoopProxy<UserEvent>>>,
    /// Live connections, at most [`MAX_CONNECTIONS`].
    pub(crate) connections: AtomicU64,
    /// Connection notices queued and not yet shown, at most [`MAX_PENDING_NOTICES`].
    pending_notices: AtomicU64,
    /// Bytes every connection holds right now: partial request lines and replies being written.
    /// The loop charges this figure to the governor's `Mcp` owner.
    pub(crate) buffered_bytes: AtomicU64,
}

impl McpShared {
    /// Shared state at `control`, waking the loop through `proxy`.
    pub(crate) fn new(control: McpControl, proxy: Option<EventLoopProxy<UserEvent>>) -> Arc<Self> {
        Arc::new(Self {
            inbox: Mutex::new(Vec::new()),
            generation: AtomicU64::new(1),
            control: AtomicU8::new(control_code(control)),
            stopped: AtomicBool::new(false),
            wake_pending: AtomicBool::new(false),
            proxy: Mutex::new(proxy),
            connections: AtomicU64::new(0),
            pending_notices: AtomicU64::new(0),
            buffered_bytes: AtomicU64::new(0),
        })
    }

    /// The current control level.
    // Ordering: control Acquire pairs with the Release store in `set_control`; the level is read alone.
    pub(crate) fn control(&self) -> McpControl {
        control_from_code(self.control.load(Ordering::Acquire))
    }

    /// Change the control level and revoke every queued request.
    // Ordering: control Release publishes the level before the generation AcqRel bump revokes requests.
    pub(crate) fn set_control(&self, control: McpControl) {
        self.control.store(control_code(control), Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Stop the listener and every connection, and revoke every queued request.
    // Ordering: stopped Release publishes the stop before the generation AcqRel bump; readers Acquire both.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
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
    fn wake(&self) -> bool {
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
            reply,
        });
        let _ = self.wake();
    }

    /// Mark one queued notice as shown.
    // Ordering: pending_notices AcqRel pairs with the admitting increment in `notify`.
    pub(crate) fn notice_shown(&self) {
        self.pending_notices.fetch_sub(1, Ordering::AcqRel);
    }

    /// Take every queued request; the loop calls this when woken.
    // Ordering: wake_pending Release clears the flag before the drain, so a request pushed after it
    // sends a new wake-up.
    pub(crate) fn take_requests(&self) -> Vec<LoopRequest> {
        self.wake_pending.store(false, Ordering::Release);
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
}

impl CallScope {
    /// Admit a call now, on `shared`, for a connection whose close flag is `closed`.
    // Ordering: generation Acquire reads the value `set_control` and `stop` publish with AcqRel.
    pub(crate) fn admit(shared: &McpShared, closed: &Arc<AtomicBool>) -> Self {
        Self { closed: Arc::clone(closed), generation: shared.generation.load(Ordering::Acquire) }
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

/// Serve one connection until it closes or the server stops: a reader thread parses lines and
/// answers what needs no app state, and a worker thread runs tool calls one at a time. Returns the
/// name the client gave in `initialize`, if it did.
// Ordering: closed Release publishes the close to every waiter that Acquires it.
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
    let calls = CallSlot { queue: calls, in_flight, closed: Arc::clone(&closed) };
    read_loop(&shared, transport.reader, &writer, &caller, &calls, &closed);
    closed.store(true, Ordering::Release);
    let client = caller.lock().client.clone();
    client
}

/// The worker: run admitted tool calls one at a time and write each reply.
// Ordering: in_flight Release frees the call slot only after the reply is written.
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
        let result = tools_read::call(shared, &snapshot, &call.params, &call.scope);
        write_line(shared, writer, &jsonrpc::response(&call.id, result), closed);
        // The call's arguments stop being charged once its reply is written.
        drop(call);
        in_flight.store(false, Ordering::Release);
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
}

impl AdmittedCall {
    /// Admit a call now: its scope takes the current generation and its arguments are charged.
    fn admit(shared: &Arc<McpShared>, closed: &Arc<AtomicBool>, id: Value, params: Value) -> Self {
        let bytes = params.to_string().len();
        shared.hold_bytes(bytes);
        let scope = CallScope::admit(shared, closed);
        Self { id, params, scope, shared: Arc::clone(shared), bytes }
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
    /// Set from admission until the worker has written the reply.
    in_flight: Arc<AtomicBool>,
    /// The connection's close flag, which every admitted call carries.
    closed: Arc<AtomicBool>,
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
            let Some(reply) = answer_request(shared, &id, &method, params, caller, calls) else {
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
            let call = AdmittedCall::admit(shared, &calls.closed, id.clone(), params);
            let _ = calls.queue.try_send(call);
            return None;
        }
        _ => jsonrpc::error_response(id, jsonrpc::METHOD_NOT_FOUND, "method not found"),
    };
    Some(reply)
}

/// Write one line to the client; a failed or timed-out write marks the connection closed.
// Ordering: closed Release publishes the close to the reader and every waiter.
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
        closed.store(true, Ordering::Release);
    }
}

/// Run `serve_connection` on a new thread, counting it against [`MAX_CONNECTIONS`]; a connection
/// past the limit is dropped at once.
// Ordering: connections AcqRel makes the admit decision and the release atomic.
pub(crate) fn spawn_connection(shared: &Arc<McpShared>, transport: Transport) {
    if shared.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS as u64 {
        // When: `connections` was already at `MAX_CONNECTIONS`, this one is refused.
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

/// A resolved pane over a fresh parser with `scrollback` lines of history, as the loop hands one
/// to a connection.
#[cfg(test)]
pub(crate) fn test_entry(cols: u16, rows: u16, scrollback: usize) -> PaneEntry {
    let mut grid = sonicterm_grid::grid::Grid::new(cols, rows);
    grid.set_scrollback_limit(scrollback);
    PaneEntry {
        pane_id: 7,
        window_index: 0,
        tab_index: 0,
        active: true,
        readonly: false,
        agent_owned: false,
        parser: Arc::new(Mutex::new(Parser::new_with_staging_pool(
            grid,
            None,
            sonicterm_vt::vt::CaptureStagingPool::new(),
        ))),
        output_generation: Arc::new(AtomicU64::new(0)),
        exit_status: Arc::new(OnceLock::new()),
    }
}

/// Lines of a `BufRead`, used by the Unix endpoint tests to read replies.
#[cfg(all(test, unix))]
pub(crate) fn read_reply_line(reader: &mut impl std::io::BufRead) -> Option<Value> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim_end()).ok()
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;
