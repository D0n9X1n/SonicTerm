//! sonicterm-mux server: owns PTYs across client disconnects.
//!
//! v0.1 design notes:
//!
//! - Single in-flight client per server process (Attach replaces any prior
//!   subscriber). Multi-client fanout is FUTURE work.
//! - Each `Spawn` creates a fresh `Session` containing one `Pane`. Multi-pane
//!   sessions / pane splits inside a session are FUTURE work; the protocol
//!   already carries the distinction so clients can grow into it.
//! - Per-pane replay buffer: ring of the last `REPLAY_CAP` bytes (256 KiB).
//!   On Attach the server flushes the entire buffer to the client before
//!   resuming live forwarding. That is good enough to restore the visible
//!   screen of any reasonable shell prompt; a true scrollback-aware replay
//!   is FUTURE work.

use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use sonicterm_io::pty::PtyHandle;

use crate::proto::{ClientMsg, PaneId, PaneInfo, ServerMsg, SessionId, SessionInfo};

/// Replay buffer cap per pane.
pub const REPLAY_CAP: usize = 256 * 1024;

/// Per-client subscriber channel capacity. Bounded so a runaway or
/// malicious PTY cannot OOM the server by outpacing a slow / wedged
/// consumer. When the channel is full we drop the OLDEST queued message
/// so the freshest output still reaches the client. 4096 frames @ ~8 KiB
/// each is a soft ceiling of ~32 MiB per attached pane.
pub const CHANNEL_CAP: usize = 4096;

/// One subscriber's bounded mailbox sender.
#[derive(Clone)]
pub struct SubscriberSink {
    tx: Sender<ServerMsg>,
}

impl SubscriberSink {
    /// Wrap a paired `tx`/`rx` into a subscriber sink. The receiver argument
    /// is retained for source compatibility; ownership remains with the client.
    pub fn new(tx: Sender<ServerMsg>, _rx: Receiver<ServerMsg>) -> Self {
        Self { tx }
    }

    /// Try to enqueue `msg`. If the mailbox is full, new PTY output is
    /// dropped while control responses return an error. Existing queued
    /// control messages are never evicted.
    pub fn send_drop_oldest(&self, msg: ServerMsg) -> Result<()> {
        if matches!(msg, ServerMsg::Output { .. })
            && self.tx.capacity().is_some_and(|capacity| {
                self.tx.len() >= capacity.saturating_sub(1)
            })
        {
            return Ok(());
        }
        match self.tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(ServerMsg::Output { .. })) => Ok(()),
            Err(TrySendError::Full(_)) => {
                Err(anyhow!("subscriber mailbox full; control message not queued"))
            }
            Err(TrySendError::Disconnected(_)) => Err(anyhow!("subscriber disconnected")),
        }
    }

    /// Deliver a control message without eviction. This may wait for a slow
    /// client to consume bounded output, but it never drops pane termination.
    pub fn send_control(&self, msg: ServerMsg) -> Result<()> {
        self.tx.send(msg).map_err(|_| anyhow!("subscriber disconnected"))
    }
}

struct Pane {
    id: PaneId,
    cmd: String,
    cols: u16,
    rows: u16,
    /// PTY spawned through the shared `sonicterm-io` seam. Owns the child,
    /// the reader/writer threads, the deduped resize closure, and the
    /// robust kill-on-Drop. mux writes via `pty.in_tx` and resizes via
    /// `pty.resize`; its own reader thread consumes `pty.out_rx`.
    pty: PtyHandle,
    /// Most-recent bytes, cap = REPLAY_CAP. Bounded ring (FIFO trim from
    /// front when over capacity).
    replay: Arc<Mutex<VecDeque<u8>>>,
    /// Live subscriber (the attached client). When None, output is only
    /// appended to the replay buffer.
    subscriber: Arc<Mutex<Option<SubscriberSink>>>,
    /// Signals the replay reader thread to wind down on pane kill.
    alive: Arc<AtomicBool>,
    _reader: JoinHandle<()>,
}

impl Pane {
    fn info(&self) -> PaneInfo {
        PaneInfo { id: self.id, cmd: self.cmd.clone(), cols: self.cols, rows: self.rows }
    }

    fn kill(&self) {
        self.alive.store(false, Ordering::Release);
        self.pty.kill();
    }
}

struct Session {
    id: SessionId,
    panes: HashMap<PaneId, Pane>,
}

/// The server's mutable state. Held inside an `Arc<Mutex<_>>` so the
/// connection-handler thread and the pane reader threads can both touch it.
pub struct ServerState {
    next_session: AtomicU64,
    next_pane: AtomicU64,
    sessions: Mutex<HashMap<SessionId, Session>>,
    /// Currently attached session, if any.
    attached: Mutex<Option<SessionId>>,
}

impl ServerState {
    /// Build an empty server state wrapped in an `Arc` ready for sharing across
    /// the connection handler and per-pane reader/writer threads.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            next_session: AtomicU64::new(1),
            next_pane: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
            attached: Mutex::new(None),
        })
    }

    fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .lock()
            .values()
            .map(|s| SessionInfo { id: s.id, pane_count: s.panes.len() })
            .collect()
    }

    /// Number of live sessions (test helper / admin introspection).
    pub fn session_count(&self) -> usize {
        self.sessions.lock().len()
    }

    /// Spawn a new pane in a fresh session and return (session_id, pane_id).
    pub fn spawn(&self, cmd: &str, cols: u16, rows: u16) -> Result<(SessionId, PaneId)> {
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let pane_id = self.next_pane.fetch_add(1, Ordering::Relaxed);
        let pane = build_pane(pane_id, cmd, cols, rows)?;
        let mut sessions = self.sessions.lock();
        let session = Session { id: session_id, panes: HashMap::from([(pane_id, pane)]) };
        sessions.insert(session_id, session);
        Ok((session_id, pane_id))
    }

    /// Subscribe `tx` as the new live consumer for every pane in
    /// `session_id`. Returns the pane info list to send in AttachOk.
    /// Also drains each pane's replay buffer to the new subscriber.
    pub fn attach(&self, session_id: SessionId, sink: SubscriberSink) -> Result<Vec<PaneInfo>> {
        let sessions = self.sessions.lock();
        let session =
            sessions.get(&session_id).ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        let mut infos = Vec::new();
        for pane in session.panes.values() {
            infos.push(pane.info());
            // Replay first, then install subscriber, so the live consumer
            // sees replay-then-live in order.
            let replay_bytes: Vec<u8> = pane.replay.lock().iter().copied().collect();
            if !replay_bytes.is_empty() {
                let _ = sink
                    .send_drop_oldest(ServerMsg::Output { pane_id: pane.id, bytes: replay_bytes });
            }
            *pane.subscriber.lock() = Some(sink.clone());
        }
        *self.attached.lock() = Some(session_id);
        Ok(infos)
    }

    /// Drop the current attachment: clear each pane's subscriber slot so live
    /// output again only accumulates into the replay ring.
    pub fn detach(&self) {
        let attached = self.attached.lock().take();
        if let Some(sid) = attached {
            if let Some(session) = self.sessions.lock().get(&sid) {
                for pane in session.panes.values() {
                    *pane.subscriber.lock() = None;
                }
            }
        }
    }

    /// Wire `tx` as the subscriber for every pane in `session_id` ONLY if
    /// no client is currently attached. Used by the auto-subscribe-on-Spawn
    /// convenience path so a freshly-spawned pane streams its output back
    /// to the spawner without requiring an explicit Attach.
    pub fn subscribe_if_unattached(&self, session_id: SessionId, sink: SubscriberSink) {
        let mut attached = self.attached.lock();
        if attached.is_some() {
            return;
        }
        if let Some(session) = self.sessions.lock().get(&session_id) {
            for pane in session.panes.values() {
                *pane.subscriber.lock() = Some(sink.clone());
            }
            *attached = Some(session_id);
        }
    }

    /// Forward client-side keystrokes / paste bytes to the named pane's PTY
    /// writer thread. Errors if the pane is unknown or already torn down.
    pub fn input(&self, pane_id: PaneId, bytes: Vec<u8>) -> Result<()> {
        if !sonicterm_io::pty::pty_input_message_allowed(bytes.len()) {
            anyhow::bail!(
                "pane input message is {} bytes; maximum is {}",
                bytes.len(),
                sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES
            );
        }
        let sender = {
            let sessions = self.sessions.lock();
            find_pane(&sessions, pane_id)?.pty.in_tx.clone()
        };
        sender.try_send(bytes).map_err(|error| match error {
            crossbeam_channel::TrySendError::Full(_) => {
                anyhow!("pane writer queue is full")
            }
            crossbeam_channel::TrySendError::Disconnected(_) => {
                anyhow!("pane writer is closed")
            }
        })
    }

    /// Propagate a client-side resize to the pane's PTY via `TIOCSWINSZ`
    /// (or the Windows equivalent).
    pub fn resize(&self, pane_id: PaneId, cols: u16, rows: u16) -> Result<()> {
        let sessions = self.sessions.lock();
        let pane = find_pane(&sessions, pane_id)?;
        (pane.pty.resize)(cols, rows);
        Ok(())
    }

    /// Remove a pane from its session and SIGKILL its child. Errors if no
    /// session contains a pane with that id.
    pub fn kill_pane(&self, pane_id: PaneId) -> Result<()> {
        let mut sessions = self.sessions.lock();
        for session in sessions.values_mut() {
            if let Some(pane) = session.panes.remove(&pane_id) {
                pane.kill();
                return Ok(());
            }
        }
        Err(anyhow!("unknown pane {pane_id}"))
    }
}

fn find_pane(sessions: &HashMap<SessionId, Session>, pane_id: PaneId) -> Result<&Pane> {
    for session in sessions.values() {
        if let Some(pane) = session.panes.get(&pane_id) {
            return Ok(pane);
        }
    }
    Err(anyhow!("unknown pane {pane_id}"))
}

fn build_pane(pane_id: PaneId, cmd: &str, cols: u16, rows: u16) -> Result<Pane> {
    // Spawn through the shared sonicterm-io PTY seam. It owns the openpty,
    // the reader/writer threads, the deduped resize closure, and the robust
    // kill-on-Drop (SIGKILL + bounded reap) that the old hand-rolled path
    // lacked. It also applies the same TERM/COLORTERM/TERM_PROGRAM child
    // env as the GUI pane path via `apply_child_pty_env`.
    let pty = PtyHandle::spawn(cmd, cols, rows)?;

    let replay = Arc::new(Mutex::new(VecDeque::<u8>::with_capacity(REPLAY_CAP)));
    let subscriber: Arc<Mutex<Option<SubscriberSink>>> = Arc::new(Mutex::new(None));
    let alive = Arc::new(AtomicBool::new(true));

    // Replay reader thread: drain the PTY's output channel into the replay
    // ring + (optional) subscriber. crossbeam is MPMC, so cloning `out_rx`
    // shares the same queue; the copy left in `pty` is never polled.
    let out_rx = pty.out_rx.clone();
    let r_replay = replay.clone();
    let r_sub = subscriber.clone();
    let r_alive = alive.clone();
    let reader_thread = thread::spawn(move || {
        while r_alive.load(Ordering::Acquire) {
            // Recv blocks until data or channel close. On pane kill the PTY
            // child dies, its reader thread drops the sender, and this recv
            // returns Err — unblocking the loop even without the alive flag.
            let Ok(chunk) = out_rx.recv() else { break };
            {
                let mut rb = r_replay.lock();
                rb.extend(chunk.iter().copied());
                while rb.len() > REPLAY_CAP {
                    rb.pop_front();
                }
            }
            let sub = r_sub.lock().clone();
            if let Some(sink) = sub {
                let _ =
                    sink.send_drop_oldest(ServerMsg::Output { pane_id, bytes: chunk.to_vec() });
            }
        }
        let sub = r_sub.lock().clone();
        if let Some(sink) = sub {
            let _ = sink.send_control(ServerMsg::Exit { pane_id });
        }
    });

    Ok(Pane {
        id: pane_id,
        cmd: cmd.to_string(),
        cols,
        rows,
        pty,
        replay,
        subscriber,
        alive,
        _reader: reader_thread,
    })
}

/// Handle one connected client: a request reader loop on the input stream,
/// and a forwarder thread that drains server-side messages onto the output
/// stream. Both halves share the same duplex stream via `try_clone`.
pub fn handle_connection<S>(state: Arc<ServerState>, mut read_half: S, write_half: S) -> Result<()>
where
    S: Read + Write + Send + 'static,
{
    let (tx, rx): (Sender<ServerMsg>, Receiver<ServerMsg>) =
        crossbeam_channel::bounded(CHANNEL_CAP);
    let sink = SubscriberSink::new(tx.clone(), rx.clone());

    // Writer thread: drains rx -> stream.
    let mut write_half = write_half;
    let rx_writer = rx.clone();
    let writer_thread = thread::spawn(move || {
        while let Ok(msg) = rx_writer.recv() {
            if crate::frame::write_frame(&mut write_half, &msg).is_err() {
                break;
            }
        }
    });

    // Request loop on this thread.
    while let Ok(msg) = crate::frame::read_frame::<_, ClientMsg>(&mut read_half) {
        match msg {
            ClientMsg::ListSessions => {
                let _ = tx.send(ServerMsg::Sessions(state.list_sessions()));
            }
            ClientMsg::Attach(sid) => match state.attach(sid, sink.clone()) {
                Ok(panes) => {
                    let _ = tx.send(ServerMsg::AttachOk { session_id: sid, panes });
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error(e.to_string()));
                }
            },
            ClientMsg::Detach => {
                state.detach();
            }
            ClientMsg::Spawn { cmd, cols, rows } => match state.spawn(&cmd, cols, rows) {
                Ok((sid, pid)) => {
                    // Convenience: if the client isn't yet attached to any
                    // session, auto-subscribe them to the freshly spawned
                    // one. Matches the natural "I spawned it, I want its
                    // output" flow without forcing a separate Attach.
                    state.subscribe_if_unattached(sid, sink.clone());
                    let _ = tx.send(ServerMsg::Spawned { session_id: sid, pane_id: pid });
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error(e.to_string()));
                }
            },
            ClientMsg::Input { pane_id, bytes } => {
                if let Err(e) = state.input(pane_id, bytes) {
                    let _ = tx.send(ServerMsg::Error(e.to_string()));
                }
            }
            ClientMsg::Resize { pane_id, cols, rows } => {
                if let Err(e) = state.resize(pane_id, cols, rows) {
                    let _ = tx.send(ServerMsg::Error(e.to_string()));
                }
            }
            ClientMsg::Kill { pane_id } => {
                if let Err(e) = state.kill_pane(pane_id) {
                    let _ = tx.send(ServerMsg::Error(e.to_string()));
                }
            }
        }
    }

    // Client disconnected: detach so panes stop trying to push to the
    // (now-dead) writer channel. PTYs themselves stay alive.
    //
    // CRITICAL: every clone of the bounded sender must be dropped before
    // we `join` the writer thread, otherwise the writer's `rx.recv()`
    // never observes `Disconnected` and we leak two threads per
    // client/reconnect. The senders live in three places:
    //
    //   1. the local `tx` we hold here,
    //   2. the local `sink` (which owns another `Sender` clone), and
    //   3. zero or more `Option<SubscriberSink>` slots inside each
    //      attached pane (installed by `attach` / `subscribe_if_unattached`).
    //
    // `state.detach()` clears (3). We then explicitly drop (1) and (2)
    // before the `join` so the channel actually closes.
    state.detach();
    drop(tx);
    drop(sink);
    let _ = writer_thread.join();
    Ok(())
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;
