//! End-to-end tests against a real sonic-mux daemon over a local socket.
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
use sonic_mux::{
    frame::{read_frame, write_frame},
    handle_connection,
    proto::{ClientMsg, ServerMsg},
    ServerState,
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
