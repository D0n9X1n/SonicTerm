//! End-to-end tests against a real sonicterm-mux daemon over a local socket.
//!
//! Each test starts a server thread bound to a temp-dir socket path, then
//! drives it with the same wire protocol the GUI client would use.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{unbounded, Receiver};
use interprocess::{
    local_socket::{prelude::*, GenericFilePath, ListenerOptions, Stream},
    TryClone,
};
use sonicterm_mux::{
    frame::{read_frame, write_frame},
    handle_connection,
    proto::{ClientMsg, ServerMsg},
    ServerState, SubscriberSink, CHANNEL_CAP,
};

fn spawn_daemon() -> (String, Arc<ServerState>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("mux.sock").to_string_lossy().to_string();
    // Leak the tempdir so the path stays valid for the test lifetime.
    std::mem::forget(dir);
    let _ = std::fs::remove_file(&socket);
    let name = socket.clone().to_fs_name::<GenericFilePath>().expect("fs name");
    let listener = ListenerOptions::new().name(name).create_sync().expect("listen");
    let state = ServerState::new();
    let state_clone = state.clone();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            let state = state_clone.clone();
            thread::spawn(move || {
                let writer = stream.try_clone().expect("clone");
                let _ = handle_connection(state, stream, writer);
            });
        }
    });
    (socket, state)
}

/// One client-side connection with a backgrounded reader thread that
/// drains the socket onto a crossbeam channel. The channel lets tests
/// apply real `recv_timeout` deadlines rather than getting stuck inside
/// a blocking `read_exact` on the raw stream.
struct Client {
    writer: Stream,
    rx: Receiver<ServerMsg>,
}

impl Client {
    fn connect(socket: &str) -> Self {
        let deadline = Instant::now() + Duration::from_secs(2);
        let stream = loop {
            let name = socket.to_fs_name::<GenericFilePath>().expect("fs name");
            match Stream::connect(name) {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("connect: {e}"),
            }
        };
        let mut reader = stream.try_clone().expect("clone");
        let writer = stream;
        let (tx, rx) = unbounded::<ServerMsg>();
        thread::spawn(move || {
            while let Ok(m) = read_frame::<_, ServerMsg>(&mut reader) {
                if tx.send(m).is_err() {
                    break;
                }
            }
        });
        Self { writer, rx }
    }

    fn send(&mut self, m: ClientMsg) {
        write_frame(&mut self.writer, &m).expect("write_frame");
    }

    fn wait_for<F: FnMut(&ServerMsg) -> bool>(&self, mut pred: F, timeout: Duration) -> ServerMsg {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("timed out waiting for ServerMsg");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(m) => {
                    if pred(&m) {
                        return m;
                    }
                }
                Err(_) => panic!("timed out waiting for ServerMsg"),
            }
        }
    }

    /// Accumulate bytes from every Output frame until `needle` appears or
    /// the timeout fires. Returns the accumulated buffer.
    fn drain_for_bytes(&self, needle: &[u8], timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut acc = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return acc;
            }
            match self.rx.recv_timeout(remaining) {
                Ok(ServerMsg::Output { bytes, .. }) => {
                    acc.extend_from_slice(&bytes);
                    if acc.windows(needle.len()).any(|w| w == needle) {
                        return acc;
                    }
                }
                Ok(_) => {}
                Err(_) => return acc,
            }
        }
    }
}

#[test]
#[cfg_attr(windows, ignore = "named-pipe path not yet wired on Windows mux daemon")]
fn spawn_input_output_roundtrip() {
    let (sock, _state) = spawn_daemon();
    let mut c = Client::connect(&sock);

    c.send(ClientMsg::Spawn { cmd: "/bin/sh".into(), cols: 80, rows: 24 });
    let mut pane_id = 0u64;
    c.wait_for(
        |m| match m {
            ServerMsg::Spawned { pane_id: pid, .. } => {
                pane_id = *pid;
                true
            }
            _ => false,
        },
        Duration::from_secs(2),
    );
    assert!(pane_id > 0);

    c.send(ClientMsg::Input { pane_id, bytes: b"echo SONIC_MUX_MARKER\n".to_vec() });
    let seen = c.drain_for_bytes(b"SONIC_MUX_MARKER", Duration::from_secs(5));
    assert!(
        seen.windows(b"SONIC_MUX_MARKER".len()).any(|w| w == b"SONIC_MUX_MARKER"),
        "expected marker in output; got {} bytes",
        seen.len()
    );
}

#[test]
#[cfg_attr(windows, ignore = "named-pipe path not yet wired on Windows mux daemon")]
fn detach_then_reattach_replays_buffered_output() {
    let (sock, state) = spawn_daemon();
    let mut c1 = Client::connect(&sock);

    c1.send(ClientMsg::Spawn { cmd: "/bin/sh".into(), cols: 80, rows: 24 });
    let mut session_id = 0u64;
    let mut pane_id = 0u64;
    c1.wait_for(
        |m| match m {
            ServerMsg::Spawned { session_id: sid, pane_id: pid } => {
                session_id = *sid;
                pane_id = *pid;
                true
            }
            _ => false,
        },
        Duration::from_secs(2),
    );

    c1.send(ClientMsg::Input { pane_id, bytes: b"echo BEFORE_DETACH\n".to_vec() });
    let acc = c1.drain_for_bytes(b"BEFORE_DETACH", Duration::from_secs(5));
    assert!(
        acc.windows(b"BEFORE_DETACH".len()).any(|w| w == b"BEFORE_DETACH"),
        "first client must see live output"
    );

    // Drop the client connection entirely (simulates GUI quit). The server
    // must keep the PTY alive.
    drop(c1);
    thread::sleep(Duration::from_millis(200));
    assert!(state.session_count() >= 1, "session must survive client drop");

    // Reconnect and re-Attach. Replay buffer should still contain the
    // BEFORE_DETACH bytes.
    let mut c2 = Client::connect(&sock);
    c2.send(ClientMsg::Attach(session_id));
    let replay = c2.drain_for_bytes(b"BEFORE_DETACH", Duration::from_secs(5));
    assert!(
        replay.windows(b"BEFORE_DETACH".len()).any(|w| w == b"BEFORE_DETACH"),
        "expected replay to contain prior output"
    );
}

#[test]
#[cfg_attr(windows, ignore = "named-pipe path not yet wired on Windows mux daemon")]
fn server_keeps_session_after_client_drop() {
    let (sock, state) = spawn_daemon();
    let mut c = Client::connect(&sock);
    c.send(ClientMsg::Spawn { cmd: "/bin/sh".into(), cols: 80, rows: 24 });
    c.wait_for(|m| matches!(m, ServerMsg::Spawned { .. }), Duration::from_secs(2));
    let before = state.session_count();
    assert!(before >= 1);

    drop(c);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(state.session_count(), before, "session must survive client drop");
}

#[test]
#[cfg_attr(windows, ignore = "named-pipe path not yet wired on Windows mux daemon")]
fn list_sessions_round_trip() {
    let (sock, _state) = spawn_daemon();
    let mut c = Client::connect(&sock);

    c.send(ClientMsg::ListSessions);
    let first = c.wait_for(|m| matches!(m, ServerMsg::Sessions(_)), Duration::from_secs(2));
    match first {
        ServerMsg::Sessions(v) => assert_eq!(v.len(), 0),
        other => panic!("unexpected: {other:?}"),
    }

    for _ in 0..2 {
        c.send(ClientMsg::Spawn { cmd: "/bin/sh".into(), cols: 80, rows: 24 });
        c.wait_for(|m| matches!(m, ServerMsg::Spawned { .. }), Duration::from_secs(2));
    }
    c.send(ClientMsg::ListSessions);
    let second = c.wait_for(|m| matches!(m, ServerMsg::Sessions(_)), Duration::from_secs(2));
    match second {
        ServerMsg::Sessions(v) => assert_eq!(v.len(), 2),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn framing_round_trip_in_memory() {
    use std::io::{Cursor, Read};
    let mut buf = Vec::new();
    write_frame(&mut buf, &ClientMsg::ListSessions).unwrap();
    write_frame(&mut buf, &ClientMsg::Input { pane_id: 7, bytes: b"hello".to_vec() }).unwrap();
    let mut cur = Cursor::new(&buf);
    let m1: ClientMsg = read_frame(&mut cur).unwrap();
    assert!(matches!(m1, ClientMsg::ListSessions));
    let m2: ClientMsg = read_frame(&mut cur).unwrap();
    match m2 {
        ClientMsg::Input { pane_id, bytes } => {
            assert_eq!(pane_id, 7);
            assert_eq!(&bytes, b"hello");
        }
        other => panic!("unexpected: {other:?}"),
    }
    let mut leftover = [0u8; 1];
    assert_eq!(cur.read(&mut leftover).unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn daemon_socket_is_user_only_0600() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("perm.sock");
    let bin = env!("CARGO_BIN_EXE_sonic-mux");
    let mut child = Command::new(bin)
        .args(["daemon", "--socket"])
        .arg(&socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sonicterm-mux daemon");

    // Wait until the socket file appears (daemon has bound + chmod'd it).
    // Read stderr in the background so the child doesn't block on a full
    // pipe; we want the "listening" log line as confirmation too.
    if let Some(err) = child.stderr.take() {
        thread::spawn(move || {
            let mut buf = BufReader::new(err);
            let mut line = String::new();
            while buf.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                line.clear();
            }
        });
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "daemon never bound the socket");

    let meta = std::fs::metadata(&socket).expect("stat socket");
    let mode = meta.permissions().mode() & 0o777;
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        mode, 0o600,
        "socket must be owner-only (0600); world-accessible socket is a security hole. got {mode:o}"
    );
}

/// Regression: when a client disconnects, `handle_connection` must
/// return promptly. Earlier `handle_connection` held a `SubscriberSink`
/// (which owns a clone of the bounded-channel `Sender`) live across the
/// `writer_thread.join()`. That kept the channel open, so the writer's
/// `rx.recv()` never observed `Disconnected`, and the connection-handler
/// thread blocked forever joining a thread that would never exit. Two
/// threads leaked per client/reconnect cycle.
///
/// We exercise the real `handle_connection` over a UnixStream pair: spawn
/// a session, attach, then drop the client end. The handler must return
/// within 1s.
#[cfg(unix)]
#[test]
fn handle_connection_returns_after_client_disconnect() {
    use std::os::unix::net::UnixStream;

    let (client_end, server_end) = UnixStream::pair().expect("socket pair");
    let server_write = server_end.try_clone().expect("clone server end");
    let state = ServerState::new();

    // Pre-seed a session so Attach has something real to subscribe to;
    // this installs a SubscriberSink in the pane's subscriber slot, which
    // is exactly the leak pathway we are guarding against.
    let (sid, _pid) = state.spawn("/bin/sh", 80, 24).expect("spawn");

    let state_clone = state.clone();
    let handler = thread::spawn(move || handle_connection(state_clone, server_end, server_write));

    // Drive a real Attach so the SubscriberSink is installed in the pane.
    let mut writer = client_end.try_clone().expect("clone client end");
    write_frame(&mut writer, &ClientMsg::Attach(sid)).expect("write attach");

    // Give the server a moment to wire the subscriber in.
    thread::sleep(Duration::from_millis(100));

    // Simulate GUI quit: drop BOTH halves of the client side. The handler's
    // read loop must observe EOF, run cleanup, drop its sender clones, and
    // exit. We then join with a deadline.
    drop(writer);
    drop(client_end);

    let deadline = Instant::now() + Duration::from_secs(1);
    while !handler.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        handler.is_finished(),
        "handle_connection failed to return within 1s after client disconnect; \
         writer_thread.join() is hanging because a SubscriberSink clone is \
         still keeping the bounded channel alive"
    );
    let _ = handler.join();
}

#[test]
fn subscriber_channel_is_bounded_and_drops_oldest() {
    // Build a sink with a small capacity and shove far more messages in
    // than fit. The channel must never exceed CHANNEL_CAP and the newest
    // messages must be the ones that survive (drop-OLDEST policy keeps
    // the freshest output flowing to the client).
    use crossbeam_channel::bounded;
    let (tx, rx) = bounded::<ServerMsg>(CHANNEL_CAP);
    let sink = SubscriberSink::new(tx, rx.clone());

    // Track peak depth as a memory-bound proxy. Without backpressure
    // (unbounded channel) this would grow without limit.
    let mut peak = 0usize;
    let total = CHANNEL_CAP * 4;
    for i in 0..total {
        sink.send_drop_oldest(ServerMsg::Output {
            pane_id: 1,
            bytes: (i as u64).to_le_bytes().to_vec(),
        })
        .expect("sink open");
        peak = peak.max(rx.len());
    }

    assert!(peak <= CHANNEL_CAP, "channel grew past cap: peak={peak} cap={CHANNEL_CAP}");
    assert_eq!(rx.len(), CHANNEL_CAP, "channel should be at cap after flood");

    // The last value pushed must be retained (drop-OLDEST, not newest).
    let mut last_seen: Option<u64> = None;
    while let Ok(ServerMsg::Output { bytes, .. }) = rx.try_recv() {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes);
        last_seen = Some(u64::from_le_bytes(arr));
    }
    assert_eq!(
        last_seen,
        Some((total - 1) as u64),
        "newest message must survive; drop-OLDEST policy violated"
    );
}
