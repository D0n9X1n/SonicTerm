//! Pins one connection end to end over an in-memory transport: the handshake and tool list, the
//! preamble's self pane, JSON-RPC errors, one call at a time, bounded reads of a held parser, the
//! connection cap, and cancellation when the client leaves or the server stops.

use std::io::{self, BufReader, ErrorKind};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};

use serde_json::json;

use super::*;

/// The reading end of an in-memory byte pipe; an empty read waits [`READ_POLL`] then reports
/// `TimedOut`, and a dropped sender reads as end of stream, like the real endpoints.
struct ChannelReader {
    /// Chunks from the other end.
    chunks: Receiver<Vec<u8>>,
    /// The unread rest of the current chunk.
    pending: Vec<u8>,
}

impl Read for ChannelReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            match self.chunks.recv_timeout(READ_POLL) {
                Ok(chunk) => self.pending = chunk,
                Err(RecvTimeoutError::Timeout) => return Err(ErrorKind::TimedOut.into()),
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let count = buffer.len().min(self.pending.len());
        buffer[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

/// The writing end of an in-memory byte pipe; writing after the reader is gone fails.
struct ChannelWriter(Sender<Vec<u8>>);

impl Write for ChannelWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.send(buffer.to_vec()).map_err(|_| io::Error::from(ErrorKind::BrokenPipe))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The client's side of one in-memory connection.
pub(super) struct Client {
    /// Bytes to the server; `None` once the client hung up.
    to_server: Option<ChannelWriter>,
    /// Replies from the server.
    from_server: BufReader<ChannelReader>,
}

impl Client {
    /// Send one JSON value as a line.
    pub(super) fn send(&mut self, value: &Value) {
        let mut line = serde_json::to_vec(value).unwrap();
        line.push(b'\n');
        self.to_server.as_mut().expect("still connected").write_all(&line).unwrap();
    }

    /// The next reply, waiting through read timeouts for up to five seconds.
    pub(super) fn reply(&mut self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut line = String::new();
            match std::io::BufRead::read_line(&mut self.from_server, &mut line) {
                Ok(0) => panic!("the server closed the connection"),
                Ok(_) => return serde_json::from_str(line.trim_end()).unwrap(),
                Err(error) if error.kind() == ErrorKind::TimedOut && Instant::now() < deadline => {
                    continue
                }
                Err(error) => panic!("no reply: {error}"),
            }
        }
    }

    /// Whether the server closed its end within five seconds.
    fn sees_eof(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let mut line = String::new();
            match std::io::BufRead::read_line(&mut self.from_server, &mut line) {
                Ok(0) => return true,
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        false
    }
}

/// A connected client and the server-side transport for it.
pub(super) fn connection() -> (Client, Transport) {
    let (client_tx, server_rx) = channel();
    let (server_tx, client_rx) = channel();
    let client = Client {
        to_server: Some(ChannelWriter(client_tx)),
        from_server: BufReader::new(ChannelReader { chunks: client_rx, pending: Vec::new() }),
    };
    let transport = Transport {
        reader: Box::new(ChannelReader { chunks: server_rx, pending: Vec::new() }),
        writer: Box::new(ChannelWriter(server_tx)),
    };
    (client, transport)
}

/// A JSON-RPC request line.
pub(super) fn call(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// A `tools/call` request for `name` with `arguments`.
pub(super) fn tool(id: u64, name: &str, arguments: Value) -> Value {
    call(id, "tools/call", json!({ "name": name, "arguments": arguments }))
}

/// Play the event loop for `shared`: answer every queued request with `entry` until stopped.
fn fake_loop(shared: &Arc<McpShared>, entry: PaneEntry) -> std::thread::JoinHandle<()> {
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        while !shared.is_stopped() {
            for request in shared.take_requests() {
                let reply = match request.call {
                    LoopCall::ListPanes => LoopReply::Panes(vec![entry.clone()]),
                    LoopCall::ResolvePane { pane_id } if pane_id == entry.pane_id => {
                        LoopReply::Pane(entry.clone())
                    }
                    LoopCall::ResolvePane { pane_id } => {
                        LoopReply::Refused("pane_gone", format!("no pane {pane_id}"))
                    }
                    // A notice wants no reply; the fake loop shows nothing.
                    LoopCall::Notice { .. } => continue,
                    // The read-path tests never send control calls.
                    _ => LoopReply::Refused(
                        "unavailable",
                        "the fake loop runs no control calls".into(),
                    ),
                };
                let _ = request.reply.try_send(reply);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })
}

/// Wait until `shared` reports `count` live connections, for up to five seconds.
fn wait_for_connections(shared: &McpShared, count: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if shared.connections.load(Ordering::Acquire) == count {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// `initialize` and `tools/list` round-trip; `ping` answers empty; unknown methods and a call
/// without a tool name get their JSON-RPC errors.
#[test]
fn the_handshake_and_errors_round_trip() {
    let shared = McpShared::new(McpControl::Agent, None);
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    client.send(&call(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "clientInfo": { "name": "test" } }),
    ));
    assert_eq!(client.reply()["result"]["protocolVersion"], "2025-06-18");
    client.send(&call(2, "tools/list", json!({})));
    let tools = client.reply()["result"]["tools"].as_array().unwrap().len();
    assert!(tools >= 3);
    client.send(&call(3, "ping", json!({})));
    assert_eq!(client.reply()["result"], json!({}));
    client.send(&call(4, "resources/list", json!({})));
    assert_eq!(client.reply()["error"]["code"], jsonrpc::METHOD_NOT_FOUND);
    client.send(&call(5, "tools/call", json!({ "arguments": {} })));
    assert_eq!(client.reply()["error"]["code"], jsonrpc::INVALID_PARAMS);
    shared.stop();
}

/// The bridge preamble names the agent's own pane, which `list_panes` reports while that pane
/// exists; it grants nothing else.
#[test]
fn the_preamble_names_the_self_pane() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let pane_id = entry.pane_id;
    let looper = fake_loop(&shared, entry);
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    client.send(&json!({ "sonicterm_bridge": 1, "pane_id": pane_id.to_string() }));
    client.send(&tool(1, "list_panes", json!({})));
    let result = client.reply()["result"].clone();
    assert_eq!(result["structuredContent"]["self_pane_id"], pane_id.to_string());
    assert_eq!(result["structuredContent"]["panes"][0]["pane_id"], pane_id.to_string());
    shared.stop();
    looper.join().unwrap();
}

/// A read of a parser someone else holds returns busy and truncated within 50 ms instead of
/// waiting for the lock.
#[test]
fn a_held_parser_reads_busy_quickly() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let parser = Arc::clone(&entry.parser);
    let pane_id = entry.pane_id;
    let looper = fake_loop(&shared, entry);
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    // Resolve once first so the timed call measures only the read.
    client.send(&tool(1, "read_pane", json!({ "pane_id": pane_id })));
    client.reply();
    let held = parser.lock();
    let started = Instant::now();
    client.send(&tool(2, "read_pane", json!({ "pane_id": pane_id })));
    let reply = client.reply();
    let elapsed = started.elapsed();
    drop(held);
    let structured = &reply["result"]["structuredContent"];
    assert_eq!(structured["busy"], true);
    assert_eq!(structured["truncated"], true);
    // The parser stays held for the whole call, so only the bounded wait can end it; the bound is
    // loose because CI runners stall for tens of milliseconds.
    assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
    shared.stop();
    looper.join().unwrap();
}

/// A second call while one runs is refused at once rather than queued behind it.
#[test]
fn one_tool_call_at_a_time() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let pane_id = entry.pane_id;
    let looper = fake_loop(&shared, entry);
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    client.send(&tool(1, "wait_for", json!({ "pane_id": pane_id, "quiet_ms": 400 })));
    std::thread::sleep(Duration::from_millis(100));
    client.send(&tool(2, "read_pane", json!({ "pane_id": pane_id })));
    let first = client.reply();
    assert_eq!(first["id"], 2);
    assert_eq!(first["error"]["code"], jsonrpc::INVALID_REQUEST);
    let second = client.reply();
    assert_eq!(second["id"], 1);
    assert_eq!(second["result"]["structuredContent"]["reason"], "quiet");
    shared.stop();
    looper.join().unwrap();
}

/// A client that hangs up during a `wait_for` cancels it, and its slot is released.
#[test]
fn a_departing_client_cancels_its_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let pane_id = entry.pane_id;
    let looper = fake_loop(&shared, entry);
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    client.send(&tool(
        1,
        "wait_for",
        json!({ "pane_id": pane_id, "regex": "never", "timeout_ms": 60000 }),
    ));
    std::thread::sleep(Duration::from_millis(100));
    client.to_server = None;
    assert!(wait_for_connections(&shared, 0), "the connection outlived its client");
    shared.stop();
    looper.join().unwrap();
}

/// A request queued for a loop that never answers is abandoned when its client leaves, and the
/// connection ends without the loop doing anything.
#[test]
fn a_departing_client_abandons_a_queued_request() {
    let shared = McpShared::new(McpControl::Agent, None);
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    client.send(&tool(1, "list_panes", json!({})));
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.inbox.lock().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    client.to_server = None;
    assert!(wait_for_connections(&shared, 0));
    // The loop wakes late: the request it finds is marked closed, so it will be refused unrun.
    let queued = shared.take_requests();
    assert_eq!(queued.len(), 1);
    assert!(queued[0].closed.load(Ordering::Acquire));
}

/// Stopping the server ends a running wait and closes the connection.
#[test]
fn stopping_ends_a_running_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let pane_id = entry.pane_id;
    let looper = fake_loop(&shared, entry);
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    client.send(&tool(
        1,
        "wait_for",
        json!({ "pane_id": pane_id, "regex": "never", "timeout_ms": 60000 }),
    ));
    std::thread::sleep(Duration::from_millis(100));
    shared.stop();
    let reply = client.reply();
    // The whole call is revoked, so it answers with the refusal rather than a wait result.
    assert_eq!(reply["result"]["structuredContent"]["error"], "revoked");
    assert!(client.sees_eof());
    looper.join().unwrap();
}

/// Past [`MAX_CONNECTIONS`] a new connection is closed at once.
#[test]
fn connections_are_capped() {
    let shared = McpShared::new(McpControl::Agent, None);
    let mut clients = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        let (client, transport) = connection();
        spawn_connection(&shared, transport);
        clients.push(client);
    }
    assert!(wait_for_connections(&shared, MAX_CONNECTIONS as u64));
    let (mut refused, transport) = connection();
    spawn_connection(&shared, transport);
    assert!(refused.sees_eof());
    assert_eq!(shared.connections.load(Ordering::Acquire), MAX_CONNECTIONS as u64);
    shared.stop();
}

/// A server stopped after the loop resolved the pane, but before the read returned, answers with
/// the refusal and no pane text.
#[test]
fn a_read_revoked_after_resolution_returns_no_text() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    entry.parser.lock().advance(b"secret");
    let pane_id = entry.pane_id;
    let looper = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || loop {
            let requests = shared.take_requests();
            if let Some(request) = requests.into_iter().next() {
                // The server stops while the loop holds the resolved pane, so the stop is in place
                // before the worker can read; stopping after the reply raced the worker's recheck.
                shared.stop();
                let _ = request.reply.try_send(LoopReply::Pane(entry.clone()));
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        })
    };
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    client.send(&tool(1, "read_pane", json!({ "pane_id": pane_id })));
    let reply = client.reply();
    looper.join().unwrap();
    assert_eq!(reply["result"]["isError"], true);
    assert!(!reply.to_string().contains("secret"), "no pane text after revocation");
}

/// A call queued before a control change carries the generation it was accepted under, so the
/// change revokes it even though the worker starts it afterwards.
#[test]
fn a_queued_call_keeps_its_acceptance_generation() {
    let shared = McpShared::new(McpControl::Agent, None);
    let closed = Arc::new(AtomicBool::new(false));
    let call = AdmittedCall::admit(&shared, &closed, json!(1), json!({ "name": "list_panes" }));
    assert!(shared.buffered() > 0, "accepted arguments are charged at once");
    shared.set_control(McpControl::Off);
    assert!(call.scope.revoked(&shared));
    drop(call);
    assert_eq!(shared.buffered(), 0, "the charge goes with the call");
}

/// Repeated `initialize` on one connection is answered but announced once, and connection churn
/// never queues more than `MAX_PENDING_NOTICES` notices.
#[test]
fn notices_are_bounded() {
    let shared = McpShared::new(McpControl::Agent, None);
    let (mut client, transport) = connection();
    spawn_connection(&shared, transport);
    for id in 0..5 {
        client.send(&call(id, "initialize", json!({ "clientInfo": { "name": "spam" } })));
        assert_eq!(client.reply()["id"], id);
    }
    assert_eq!(shared.inbox.lock().len(), 1, "only the first initialize is announced");
    for _ in 0..100 {
        shared.notify("churn".into());
    }
    assert_eq!(shared.inbox.lock().len() as u64, MAX_PENDING_NOTICES);
    shared.stop();
}

/// A writer that records whether the call slot was still taken at each write.
struct SlotWatchingWriter {
    /// The connection's call slot flag.
    in_flight: Arc<AtomicBool>,
    /// One entry per write: the slot's state while the reply was going out.
    taken_at_write: Arc<Mutex<Vec<bool>>>,
}

impl Write for SlotWatchingWriter {
    // Ordering: in_flight Acquire pairs with the worker's Release store that frees the slot.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.taken_at_write.lock().push(self.in_flight.load(Ordering::Acquire));
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The worker frees the call slot before any byte of the reply goes out, so a client that sends
/// its next call as soon as it reads a reply is never refused with "one tool call at a time".
// Ordering: in_flight Release marks the slot taken, as `CallSlot::claim` does at admission.
#[test]
fn the_call_slot_is_free_before_the_reply_is_written() {
    let shared = McpShared::new(McpControl::Agent, None);
    let closed = Arc::new(AtomicBool::new(false));
    let in_flight = Arc::new(AtomicBool::new(true));
    let taken_at_write = Arc::new(Mutex::new(Vec::new()));
    let writer: Mutex<Box<dyn Write + Send>> = Mutex::new(Box::new(SlotWatchingWriter {
        in_flight: Arc::clone(&in_flight),
        taken_at_write: Arc::clone(&taken_at_write),
    }));
    let (queue, call_queue) = sync_channel(1);
    // An unknown tool answers at once without the event loop, so only the slot order is tested.
    let params = json!({ "name": "no_such_tool", "arguments": {} });
    queue.send(AdmittedCall::admit(&shared, &closed, json!(1), params)).unwrap();
    drop(queue);
    run_calls(&shared, &call_queue, &writer, &Mutex::new(Caller::default()), &closed, &in_flight);
    let writes = taken_at_write.lock().clone();
    assert!(!writes.is_empty(), "the worker wrote the reply");
    assert!(writes.iter().all(|taken| !taken), "the slot was still taken while the reply went out");
    assert!(!in_flight.load(Ordering::Acquire));
}
