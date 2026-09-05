//! sonicterm-mux server: owns PTYs across client disconnects.
//!
//! Shape of the current protocol implementation:
//!
//! - One in-flight client per server process: Attach replaces any prior
//!   subscriber rather than fanning output out to several.
//! - Each `Spawn` creates a fresh `Session` holding one `Pane`. The protocol
//!   distinguishes sessions from panes, so a client may address them
//!   separately even though the server never puts two panes in one session.
//! - Per-pane replay buffer: a ring of the last `REPLAY_CAP` bytes (256 KiB).
//!   Attach and the first live-stream gap pause output until the client resets
//!   parser state and requests one `ReplaySnapshot`. Snapshot capture and live
//!   resume share a lock boundary, so no later bytes cross the gap silently.
//!   The ring stores raw bytes, not reconstructed scrollback.

use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Weak,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, SendTimeoutError, Sender, TrySendError};
use parking_lot::Mutex;
use sonicterm_io::pty::PtyHandle;

use crate::proto::{ClientMsg, PaneId, PaneInfo, ServerMsg, SessionId, SessionInfo};

/// Replay buffer cap per pane.
pub const REPLAY_CAP: usize = 256 * 1024;

/// Per-client subscriber channel capacity. Bounded so a runaway or
/// malicious PTY cannot OOM the server by outpacing a slow / wedged
/// consumer. Output stops two slots short of capacity so one recovery marker
/// and one terminal control message can still arrive. Once saturated, later
/// output is suppressed until a replay snapshot restores continuity. Every
/// byte-bearing output or replay fragment is at most 8 KiB, so all 4096 slots
/// hold at most 32 MiB of payload per attached client.
pub const CHANNEL_CAP: usize = 4096;

/// Maximum bytes stored in one queued live-output or replay-fragment payload.
pub const SUBSCRIBER_OUTPUT_FRAME_BYTES: usize = 8 * 1024;
const _: () = assert!(CHANNEL_CAP * SUBSCRIBER_OUTPUT_FRAME_BYTES <= 32 * 1024 * 1024);

const SUBSCRIBER_CONTROL_SEND_TIMEOUT: Duration = Duration::from_millis(100);
const WRITER_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(100);
const PANE_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const PANE_EXIT_DRAIN_GRACE: Duration = Duration::from_millis(100);

/// Result of attempting to queue one contiguous output payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSendResult {
    /// Every output chunk was queued without a gap.
    Queued,
    /// Delivery is paused for replay; `ResyncRequired` is queued or pending delivery.
    Lagged,
    /// The subscriber channel is disconnected.
    Disconnected,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplayPause {
    Initial,
    MarkerPending,
    MarkerQueued,
}

struct SubscriberShared {
    tx: Sender<ServerMsg>,
    replay_pauses: Mutex<HashMap<PaneId, ReplayPause>>,
}

impl SubscriberShared {
    fn retry_pending_recovery(&self) -> OutputSendResult {
        let mut pauses = self.replay_pauses.lock();
        let Some(pane_id) = pauses.iter().find_map(|(pane_id, pause)| {
            (*pause == ReplayPause::MarkerPending).then_some(*pane_id)
        }) else {
            // When: no MarkerPending pane_id exists, the writer has no control frame to retry.
            return OutputSendResult::Queued;
        };
        match self.tx.try_send(ServerMsg::ResyncRequired { pane_id }) {
            Ok(()) => {
                pauses.insert(pane_id, ReplayPause::MarkerQueued);
                OutputSendResult::Lagged
            }
            Err(TrySendError::Full(_)) => OutputSendResult::Lagged,
            Err(TrySendError::Disconnected(_)) => {
                pauses.remove(&pane_id);
                OutputSendResult::Disconnected
            }
        }
    }
}

/// One subscriber's bounded mailbox sender and per-pane continuity state.
#[derive(Clone)]
pub struct SubscriberSink {
    shared: Arc<SubscriberShared>,
}

impl SubscriberSink {
    /// Wrap a paired `tx`/`rx` into a subscriber sink. The receiver argument
    /// is retained for source compatibility; ownership remains with the client.
    pub fn new(tx: Sender<ServerMsg>, _rx: Receiver<ServerMsg>) -> Self {
        Self {
            shared: Arc::new(SubscriberShared { tx, replay_pauses: Mutex::new(HashMap::new()) }),
        }
    }

    fn same_subscriber(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    fn pause_for_replay(&self, pane_id: PaneId) {
        self.shared.replay_pauses.lock().insert(pane_id, ReplayPause::Initial);
    }

    fn send_snapshot(&self, pane_id: PaneId, bytes: Vec<u8>) -> Result<()> {
        match self.shared.replay_pauses.lock().get(&pane_id) {
            None => {
                // When: None means pane_id was never paused, so a snapshot would duplicate output.
                return Err(anyhow!("pane {pane_id} is not waiting for replay"));
            }
            Some(ReplayPause::MarkerPending) => {
                // When: MarkerPending still awaits capacity, snapshot bytes must not overtake it.
                return Err(anyhow!("pane {pane_id} recovery marker is not queued"));
            }
            Some(ReplayPause::Initial | ReplayPause::MarkerQueued) => {
                // When: Initial or MarkerQueued records a visible replay boundary, delivery may begin.
            }
        }
        let deadline = Instant::now() + SUBSCRIBER_CONTROL_SEND_TIMEOUT;
        let fragments = bytes.len().div_ceil(SUBSCRIBER_OUTPUT_FRAME_BYTES).max(1);
        for index in 0..fragments {
            let start = index * SUBSCRIBER_OUTPUT_FRAME_BYTES;
            let end = bytes.len().min(start + SUBSCRIBER_OUTPUT_FRAME_BYTES);
            let message = ServerMsg::ReplaySnapshot {
                pane_id,
                start: index == 0,
                complete: index + 1 == fragments,
                bytes: bytes[start..end].to_vec(),
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            self.shared.tx.send_timeout(message, remaining).map_err(|error| match error {
                SendTimeoutError::Timeout(_) => anyhow!(
                    "subscriber mailbox remained full for {} ms; replay snapshot not queued",
                    SUBSCRIBER_CONTROL_SEND_TIMEOUT.as_millis()
                ),
                SendTimeoutError::Disconnected(_) => anyhow!("subscriber disconnected"),
            })?;
        }
        self.shared.replay_pauses.lock().remove(&pane_id);
        Ok(())
    }

    fn mark_lagged(&self, pane_id: PaneId) -> OutputSendResult {
        let mut pauses = self.shared.replay_pauses.lock();
        match pauses.get(&pane_id) {
            Some(ReplayPause::Initial | ReplayPause::MarkerQueued) => {
                // When: Initial or MarkerQueued already requires replay, do not queue another marker.
                return OutputSendResult::Lagged;
            }
            Some(ReplayPause::MarkerPending) => {
                // When: MarkerPending still awaits capacity, retain that recovery state.
            }
            None => {
                pauses.insert(pane_id, ReplayPause::MarkerPending);
            }
        }
        match self.shared.tx.try_send(ServerMsg::ResyncRequired { pane_id }) {
            Ok(()) => {
                pauses.insert(pane_id, ReplayPause::MarkerQueued);
                OutputSendResult::Lagged
            }
            Err(TrySendError::Full(_)) => OutputSendResult::Lagged,
            Err(TrySendError::Disconnected(_)) => {
                pauses.remove(&pane_id);
                OutputSendResult::Disconnected
            }
        }
    }

    /// Queue contiguous output or mark the pane lagged on the first dropped chunk.
    pub fn send_output(&self, pane_id: PaneId, bytes: &[u8]) -> OutputSendResult {
        let pause = self.shared.replay_pauses.lock().get(&pane_id).copied();
        if let Some(pause) = pause {
            // When: pause exists, retry MarkerPending or keep suppressing live bytes.
            return if pause == ReplayPause::MarkerPending {
                self.mark_lagged(pane_id)
            } else {
                // When: pause is not MarkerPending, replay must finish before live output resumes.
                OutputSendResult::Lagged
            };
        }
        for chunk in bytes.chunks(SUBSCRIBER_OUTPUT_FRAME_BYTES) {
            // When: tx capacity reserves fewer than two slots, mark this chunk boundary lagged.
            if self
                .shared
                .tx
                .capacity()
                .is_some_and(|capacity| self.shared.tx.len() >= capacity.saturating_sub(2))
            {
                return self.mark_lagged(pane_id);
            }
            match self.shared.tx.try_send(ServerMsg::Output { pane_id, bytes: chunk.to_vec() }) {
                Ok(()) => {
                    // When: this chunk queued contiguously, continue with any remaining chunks.
                }
                Err(TrySendError::Full(_)) => {
                    // When: Full wins the send race, convert this first gap into replay.
                    return self.mark_lagged(pane_id);
                }
                Err(TrySendError::Disconnected(_)) => {
                    // When: Disconnected closes the receiver, no recovery marker can be delivered.
                    return OutputSendResult::Disconnected;
                }
            }
        }
        OutputSendResult::Queued
    }

    /// Compatibility wrapper for bounded output and non-blocking control messages.
    #[deprecated(since = "1.2.9", note = "use send_output or send_control")]
    pub fn send_drop_oldest(&self, msg: ServerMsg) -> Result<()> {
        match msg {
            ServerMsg::Output { pane_id, bytes } => match self.send_output(pane_id, &bytes) {
                OutputSendResult::Queued | OutputSendResult::Lagged => Ok(()),
                OutputSendResult::Disconnected => Err(anyhow!("subscriber disconnected")),
            },
            control => match self.shared.tx.try_send(control) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => {
                    Err(anyhow!("subscriber mailbox full; control message not queued"))
                }
                Err(TrySendError::Disconnected(_)) => Err(anyhow!("subscriber disconnected")),
            },
        }
    }

    /// Deliver a control message without eviction, waiting only for the
    /// bounded control-delivery deadline.
    pub fn send_control(&self, msg: ServerMsg) -> Result<()> {
        self.shared.tx.send_timeout(msg, SUBSCRIBER_CONTROL_SEND_TIMEOUT).map_err(|error| {
            match error {
                SendTimeoutError::Timeout(_) => anyhow!(
                    "subscriber mailbox remained full for {} ms; control message not queued",
                    SUBSCRIBER_CONTROL_SEND_TIMEOUT.as_millis()
                ),
                SendTimeoutError::Disconnected(_) => anyhow!("subscriber disconnected"),
            }
        })
    }
}

struct Pane {
    id: PaneId,
    cmd: String,
    cols: u16,
    rows: u16,
    /// PTY spawned through the shared `sonicterm-io` seam. Owns the child,
    /// the reader/writer threads, the deduped resize closure, and the
    /// robust kill-on-Drop. mux writes via `pty.input_sender()` and resizes
    /// via `pty.resize`; its own reader thread consumes `pty.out_rx`.
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

    // Ordering: alive is stored Release so the reader thread's Acquire load observes the
    // shutdown request before pty.kill tears the PTY down underneath it.
    fn kill(&self) -> Result<()> {
        self.alive.store(false, Ordering::Release);
        self.pty.kill()?;
        Ok(())
    }
}

struct Session {
    id: SessionId,
    panes: HashMap<PaneId, Pane>,
}

struct PendingPaneStart {
    session_id: SessionId,
    pane_id: PaneId,
    start_tx: Sender<()>,
}

struct Attachment {
    session_id: SessionId,
    sink: SubscriberSink,
}

impl PendingPaneStart {
    fn start(self) -> Result<()> {
        self.start_tx.send(()).map_err(|_| anyhow!("pane reader stopped before startup"))
    }
}

/// The server's mutable state. Held inside an `Arc<Mutex<_>>` so the
/// connection-handler thread and the pane reader threads can both touch it.
pub struct ServerState {
    next_session: AtomicU64,
    next_pane: AtomicU64,
    sessions: Mutex<HashMap<SessionId, Session>>,
    /// Currently attached session and the subscriber that owns it, if any.
    attached: Mutex<Option<Attachment>>,
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
    pub fn spawn(self: &Arc<Self>, cmd: &str, cols: u16, rows: u16) -> Result<(SessionId, PaneId)> {
        let pending = self.spawn_paused(cmd, cols, rows)?;
        let ids = (pending.session_id, pending.pane_id);
        pending.start()?;
        Ok(ids)
    }

    // Ordering: next_session and next_pane only mint unique ids, so Relaxed suffices — no other
    // state is published through these counters.
    fn spawn_paused(self: &Arc<Self>, cmd: &str, cols: u16, rows: u16) -> Result<PendingPaneStart> {
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let pane_id = self.next_pane.fetch_add(1, Ordering::Relaxed);
        let (pane, start_tx) = build_pane(pane_id, cmd, cols, rows, Arc::downgrade(self))?;
        let mut sessions = self.sessions.lock();
        let session = Session { id: session_id, panes: HashMap::from([(pane_id, pane)]) };
        sessions.insert(session_id, session);
        Ok(PendingPaneStart { session_id, pane_id, start_tx })
    }

    /// Subscribe `sink` to `session_id` and pause each pane until replay.
    ///
    /// The caller sends `AttachOk`, resets its parser per pane, then requests a
    /// `ReplaySnapshot`. No live bytes are presented before that handshake.
    // Lock order: sessions -> attached -> subscriber. Replacing an attachment clears only
    // subscriber slots still owned by the previous connection before installing the new sink.
    pub fn attach(&self, session_id: SessionId, sink: SubscriberSink) -> Result<Vec<PaneInfo>> {
        let sessions = self.sessions.lock();
        let infos = sessions
            .get(&session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?
            .panes
            .values()
            .map(Pane::info)
            .collect::<Vec<_>>();
        let mut attached = self.attached.lock();
        if let Some(previous) = attached.take() {
            if let Some(session) = sessions.get(&previous.session_id) {
                for pane in session.panes.values() {
                    let mut subscriber = pane.subscriber.lock();
                    if subscriber
                        .as_ref()
                        .is_some_and(|active| active.same_subscriber(&previous.sink))
                    {
                        *subscriber = None;
                    }
                }
            }
        }
        let session = sessions.get(&session_id).expect("session validated above");
        for pane in session.panes.values() {
            sink.pause_for_replay(pane.id);
            *pane.subscriber.lock() = Some(sink.clone());
        }
        *attached = Some(Attachment { session_id, sink });
        Ok(infos)
    }

    /// Queue one bounded replay snapshot and resume contiguous live output.
    pub fn replay(&self, pane_id: PaneId, requester: &SubscriberSink) -> Result<()> {
        let (replay, subscriber) = {
            let sessions = self.sessions.lock();
            let pane = find_pane(&sessions, pane_id)?;
            (Arc::clone(&pane.replay), Arc::clone(&pane.subscriber))
        };
        send_replay_snapshot(&replay, &subscriber, pane_id, requester)
    }

    /// Drop the current attachment regardless of subscriber identity.
    pub fn detach(&self) {
        let attachment = self.attached.lock().take();
        if let Some(attachment) = attachment {
            self.clear_attachment(&attachment);
        }
    }

    fn detach_subscriber(&self, requester: &SubscriberSink) {
        let attachment = {
            let mut attached = self.attached.lock();
            if attached
                .as_ref()
                .is_some_and(|attachment| attachment.sink.same_subscriber(requester))
            {
                attached.take()
            } else {
                // When: requester is stale, preserve the replacement attachment it does not own.
                None
            }
        };
        if let Some(attachment) = attachment {
            self.clear_attachment(&attachment);
        }
    }

    // Lock order: sessions -> subscriber clears only pane slots owned by attachment.
    fn clear_attachment(&self, attachment: &Attachment) {
        if let Some(session) = self.sessions.lock().get(&attachment.session_id) {
            for pane in session.panes.values() {
                let mut subscriber = pane.subscriber.lock();
                if subscriber
                    .as_ref()
                    .is_some_and(|active| active.same_subscriber(&attachment.sink))
                {
                    *subscriber = None;
                }
            }
        }
    }

    /// Wire `sink` as the subscriber for every pane in `session_id` ONLY if
    /// no client is currently attached. Used by the auto-subscribe-on-Spawn
    /// convenience path so a freshly-spawned pane streams its output back
    /// to the spawner without requiring an explicit Attach.
    // Lock order: sessions -> attached -> subscriber, matching attach so the two entry points
    // cannot deadlock against each other.
    pub fn subscribe_if_unattached(&self, session_id: SessionId, sink: SubscriberSink) {
        let sessions = self.sessions.lock();
        let mut attached = self.attached.lock();
        if attached.is_some() {
            // When: attached is already set, so a client owns the stream; the spawn convenience
            // path must not steal it.
            return;
        }
        if let Some(session) = sessions.get(&session_id) {
            // The pane can exit between spawn and this call; a missing session leaves attached
            // untouched.
            for pane in session.panes.values() {
                *pane.subscriber.lock() = Some(sink.clone());
            }
            *attached = Some(Attachment { session_id, sink });
        }
    }

    /// Forward client-side keystrokes / paste bytes to the named pane's PTY
    /// writer thread. Errors if the pane is unknown or already torn down.
    pub fn input(&self, pane_id: PaneId, bytes: Vec<u8>) -> Result<()> {
        // The size cap and the non-blocking send are one operation, not two
        // steps a caller assembles. This previously checked the cap by hand and
        // then reached for the raw sender — correct, but a copy of the rule
        // that had to stay in agreement with the original.
        let sender = {
            let sessions = self.sessions.lock();
            find_pane(&sessions, pane_id)?.pty.input_sender()
        };
        sender.send(bytes).map_err(|error| match error {
            sonicterm_io::pty::PtyInputError::MessageTooLarge(bytes) => anyhow!(
                "pane input message is {} bytes; maximum is {}",
                bytes.len(),
                sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES
            ),
            sonicterm_io::pty::PtyInputError::QueueFull(_) => {
                anyhow!("pane writer queue is full")
            }
            sonicterm_io::pty::PtyInputError::WriterDisconnected(_) => {
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
        let pane = self.take_pane(pane_id).ok_or_else(|| anyhow!("unknown pane {pane_id}"))?;
        pane.kill()
    }

    fn reap_pane(&self, pane_id: PaneId) {
        drop(self.take_pane(pane_id));
    }

    // Lock order: sessions -> attached. The sessions guard is scoped to the inner block and
    // released before attached is taken, so the two are never held together.
    fn take_pane(&self, pane_id: PaneId) -> Option<Pane> {
        let (pane, empty_session) = {
            let mut sessions = self.sessions.lock();
            let session_id = sessions.iter().find_map(|(session_id, session)| {
                session.panes.contains_key(&pane_id).then_some(*session_id)
            })?;
            let session = sessions.get_mut(&session_id).expect("session found above");
            let pane = session.panes.remove(&pane_id).expect("pane found above");
            let empty_session = session.panes.is_empty().then_some(session_id);
            if empty_session.is_some() {
                // Drop the session with its last pane so a later Attach cannot name an empty
                // entry.
                sessions.remove(&session_id);
            }
            (pane, empty_session)
        };
        if let Some(session_id) = empty_session {
            let mut attached = self.attached.lock();
            if attached.as_ref().is_some_and(|attachment| attachment.session_id == session_id) {
                // Only clear the attachment that named this session; a client that attached
                // elsewhere in the meantime keeps its own.
                *attached = None;
            }
        }
        Some(pane)
    }
}

fn find_pane(sessions: &HashMap<SessionId, Session>, pane_id: PaneId) -> Result<&Pane> {
    for session in sessions.values() {
        if let Some(pane) = session.panes.get(&pane_id) {
            // When: this session owns pane_id, so stop the scan here; pane ids are unique
            // across sessions.
            return Ok(pane);
        }
    }
    Err(anyhow!("unknown pane {pane_id}"))
}

fn notify_subscriber_exit(subscriber: &Mutex<Option<SubscriberSink>>, pane_id: PaneId) {
    let sink = subscriber.lock().take();
    if let Some(sink) = sink {
        if let Err(error) = sink.send_control(ServerMsg::Exit { pane_id }) {
            // A client that vanished during teardown is the expected disconnect path, so this
            // stays at debug rather than warn.
            tracing::debug!(
                pane_id,
                error = %error,
                "pane exit notification was not delivered; subscriber detached"
            );
        }
    }
}

fn send_reply(sink: &SubscriberSink, message: ServerMsg) -> Result<()> {
    sink.send_control(message)
}

// Lock order: replay -> subscriber keeps snapshot capture atomic with live-output resumption.
fn send_replay_snapshot(
    replay: &Mutex<VecDeque<u8>>,
    subscriber: &Mutex<Option<SubscriberSink>>,
    pane_id: PaneId,
    requester: &SubscriberSink,
) -> Result<()> {
    let replay = replay.lock();
    let subscriber = subscriber.lock();
    let active = subscriber
        .as_ref()
        .filter(|active| active.same_subscriber(requester))
        .ok_or_else(|| anyhow!("pane {pane_id} is not attached to this client"))?;
    active.send_snapshot(pane_id, replay.iter().copied().collect())
}

// Lock order: replay -> subscriber matches send_replay_snapshot across replay completion.
fn forward_pane_output<T: AsRef<[u8]>>(
    chunk: T,
    replay: &Mutex<VecDeque<u8>>,
    subscriber: &Mutex<Option<SubscriberSink>>,
    pane_id: PaneId,
) {
    let bytes = chunk.as_ref();
    let mut replay = replay.lock();
    replay.extend(bytes.iter().copied());
    while replay.len() > REPLAY_CAP {
        replay.pop_front();
    }
    let mut subscriber = subscriber.lock();
    if let Some(sink) = subscriber.as_ref() {
        if sink.send_output(pane_id, bytes) == OutputSendResult::Disconnected {
            *subscriber = None;
        }
    }
}

fn drain_ready_pane_output<T: AsRef<[u8]>>(
    out_rx: &Receiver<T>,
    replay: &Mutex<VecDeque<u8>>,
    subscriber: &Mutex<Option<SubscriberSink>>,
    pane_id: PaneId,
) {
    let ready = out_rx.len();
    for _ in 0..ready {
        let Ok(chunk) = out_rx.try_recv() else {
            // When: try_recv found the queue empty or closed, so the snapshot taken from
            // out_rx.len() is already drained.
            break;
        };
        forward_pane_output(chunk, replay, subscriber, pane_id);
    }
}

// Ordering: r_alive is loaded Acquire so this thread observes the Release store in Pane::kill
// before it stops draining and reaps the pane.
fn build_pane(
    pane_id: PaneId,
    cmd: &str,
    cols: u16,
    rows: u16,
    server: Weak<ServerState>,
) -> Result<(Pane, Sender<()>)> {
    // Spawn through the shared sonicterm-io PTY seam. It owns the openpty,
    // the reader/writer threads, the deduped resize closure, and the robust
    // kill-on-Drop (SIGKILL + bounded reap) that the old hand-rolled path
    // lacked. It also applies the same TERM/COLORTERM/TERM_PROGRAM child
    // env as the GUI pane path via `apply_child_pty_env`.
    let pty = PtyHandle::spawn(cmd, cols, rows)?;
    let child_exit = pty.child_exit_probe();

    let replay = Arc::new(Mutex::new(VecDeque::<u8>::with_capacity(REPLAY_CAP)));
    let subscriber: Arc<Mutex<Option<SubscriberSink>>> = Arc::new(Mutex::new(None));
    let alive = Arc::new(AtomicBool::new(true));
    let (start_tx, start_rx) = crossbeam_channel::bounded(1);

    // Replay reader thread: drain the PTY's output channel into the replay
    // ring + (optional) subscriber. crossbeam is MPMC, so cloning `out_rx`
    // shares the same queue; the copy left in `pty` is never polled.
    let out_rx = pty.out_rx.clone();
    let r_replay = replay.clone();
    let r_sub = subscriber.clone();
    let r_alive = alive.clone();
    let reader_thread = thread::spawn(move || {
        if start_rx.recv().is_err() {
            // When: start_rx closed before a start signal arrived, so the caller abandoned this
            // pane between build_pane and start; exit without touching the PTY.
            return;
        }
        let mut exit_probe_warned = false;
        let mut next_exit_probe = Instant::now() + PANE_EXIT_POLL_INTERVAL;
        let mut exit_drain_deadline = None;
        let child_has_exited = |exit_probe_warned: &mut bool| match child_exit.has_exited() {
            Ok(exited) => exited,
            Err(error) => {
                // A failed probe is reported as "still running" so a broken probe cannot cut a
                // live pane's output short; the warning is latched to one line per pane.
                if !*exit_probe_warned {
                    tracing::warn!(pane_id, %error, "failed to probe mux pane child exit");
                    *exit_probe_warned = true;
                }
                false
            }
        };
        while r_alive.load(Ordering::Acquire) {
            let now = Instant::now();
            if exit_drain_deadline.is_some_and(|deadline| now >= deadline) {
                // When: the exit_drain_deadline grace window elapsed, so stop waiting for
                // trailing output the exited child will never produce.
                break;
            }
            let chunk = {
                let wait = if let Some(deadline) = exit_drain_deadline {
                    // Inside the drain grace window the wait is bounded by the deadline, so no
                    // further exit probe is needed.
                    deadline.saturating_duration_since(now)
                } else {
                    // When: exit_drain_deadline is unset, so wake at next_exit_probe to test
                    // whether the child has exited.
                    next_exit_probe.saturating_duration_since(now)
                };
                match out_rx.recv_timeout(wait) {
                    Ok(chunk) => chunk,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        // When: Disconnected means the PTY reader closed out_rx, so no further
                        // output can arrive and the pane is finished.
                        break;
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        // When: Timeout fired without output, so re-test liveness and child exit
                        // before waiting again.
                        if !r_alive.load(Ordering::Acquire) {
                            // When: r_alive was cleared by Pane::kill, so stop draining.
                            break;
                        }
                        let now = Instant::now();
                        if exit_drain_deadline.is_some_and(|deadline| now >= deadline) {
                            // When: the exit_drain_deadline grace window closed while waiting.
                            break;
                        }
                        if exit_drain_deadline.is_none() && child_has_exited(&mut exit_probe_warned)
                        {
                            drain_ready_pane_output(&out_rx, &r_replay, &r_sub, pane_id);
                            exit_drain_deadline = Some(now + PANE_EXIT_DRAIN_GRACE);
                        }
                        next_exit_probe = now + PANE_EXIT_POLL_INTERVAL;
                        continue;
                    }
                }
            };
            forward_pane_output(chunk, &r_replay, &r_sub, pane_id);
            let now = Instant::now();
            if exit_drain_deadline.is_none() && now >= next_exit_probe {
                if child_has_exited(&mut exit_probe_warned) {
                    drain_ready_pane_output(&out_rx, &r_replay, &r_sub, pane_id);
                    exit_drain_deadline = Some(now + PANE_EXIT_DRAIN_GRACE);
                }
                next_exit_probe = now + PANE_EXIT_POLL_INTERVAL;
            }
        }
        notify_subscriber_exit(&r_sub, pane_id);
        if let Some(server) = server.upgrade() {
            server.reap_pane(pane_id);
        }
    });

    Ok((
        Pane {
            id: pane_id,
            cmd: cmd.to_string(),
            cols,
            rows,
            pty,
            replay,
            subscriber,
            alive,
            _reader: reader_thread,
        },
        start_tx,
    ))
}

/// Handle one connected client, running teardown with no writer-shutdown hook.
///
/// This is the three-argument form of [`handle_connection_with_shutdown`], kept
/// as a public entry point for callers that hold no transport handle to
/// interrupt. Prefer the four-argument form where one is available: without a
/// hook, a write blocked on a peer that never reads is only bounded by the
/// writer-shutdown timeout.
pub fn handle_connection<S>(state: Arc<ServerState>, read_half: S, write_half: S) -> Result<()>
where
    S: Read + Write + Send + 'static,
{
    handle_connection_with_shutdown(state, read_half, write_half, || {})
}

/// Handle one connected client with an explicit writer-shutdown hook.
///
/// The hook runs before the bounded writer join and should interrupt any
/// transport write blocked on a disconnected or non-reading peer.
pub fn handle_connection_with_shutdown<R, W, F>(
    state: Arc<ServerState>,
    mut read_half: R,
    write_half: W,
    shutdown_writer: F,
) -> Result<()>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
    F: FnOnce(),
{
    let (tx, rx): (Sender<ServerMsg>, Receiver<ServerMsg>) =
        crossbeam_channel::bounded(CHANNEL_CAP);
    let sink = SubscriberSink::new(tx.clone(), rx.clone());

    // Writer thread: drains rx -> stream.
    let mut write_half = write_half;
    let rx_writer = rx.clone();
    let subscriber_shared = Arc::downgrade(&sink.shared);
    let (writer_done_tx, writer_done_rx) = crossbeam_channel::bounded(1);
    let writer_thread = thread::spawn(move || {
        while let Ok(msg) = rx_writer.recv() {
            if crate::frame::write_frame(&mut write_half, &msg).is_err() {
                // When: write_frame failed, so the transport is broken; stop draining rather
                // than spin on a socket that will never accept bytes again.
                break;
            }
            if let Some(shared) = subscriber_shared.upgrade() {
                shared.retry_pending_recovery();
            }
        }
        let _ = writer_done_tx.send(());
    });

    // Request loop on this thread.
    let mut request_error = None;
    while let Ok(msg) = crate::frame::read_frame::<_, ClientMsg>(&mut read_half) {
        // When: ClientMsg::Spawn is the only request that creates server-side state before the
        // client can name it, so a failed reply must reap the pane rather than leak a PTY.
        let result = match msg {
            ClientMsg::ListSessions => {
                send_reply(&sink, ServerMsg::Sessions(state.list_sessions()))
            }
            ClientMsg::Attach(sid) => match state.attach(sid, sink.clone()) {
                Ok(panes) => send_reply(&sink, ServerMsg::AttachOk { session_id: sid, panes }),
                Err(e) => send_reply(&sink, ServerMsg::Error(e.to_string())),
            },
            ClientMsg::Detach => {
                state.detach_subscriber(&sink);
                Ok(())
            }
            ClientMsg::Spawn { cmd, cols, rows } => match state.spawn_paused(&cmd, cols, rows) {
                Ok(pending) => {
                    // When: spawn_paused built the pane with its reader still parked, so the
                    // client learns pid before any output can be produced.
                    let sid = pending.session_id;
                    let pid = pending.pane_id;
                    // Convenience: if the client isn't yet attached to any
                    // session, auto-subscribe them to the freshly spawned
                    // one. Matches the natural "I spawned it, I want its
                    // output" flow without forcing a separate Attach.
                    state.subscribe_if_unattached(sid, sink.clone());
                    match send_reply(&sink, ServerMsg::Spawned { session_id: sid, pane_id: pid }) {
                        Ok(()) => pending.start(),
                        Err(error) => {
                            // When: send_reply failed, so the client never learned pid and could
                            // not kill the pane itself; reap it here instead of leaking a PTY.
                            let _ = state.kill_pane(pid);
                            Err(error)
                        }
                    }
                }
                Err(e) => send_reply(&sink, ServerMsg::Error(e.to_string())),
            },
            ClientMsg::Input { pane_id, bytes } => {
                if let Err(e) = state.input(pane_id, bytes) {
                    send_reply(&sink, ServerMsg::Error(e.to_string()))
                } else {
                    // When: input succeeded, so the client is owed no reply and the request
                    // loop continues.
                    Ok(())
                }
            }
            ClientMsg::Resize { pane_id, cols, rows } => {
                if let Err(e) = state.resize(pane_id, cols, rows) {
                    send_reply(&sink, ServerMsg::Error(e.to_string()))
                } else {
                    // When: resize succeeded, so no reply frame is owed.
                    Ok(())
                }
            }
            ClientMsg::Kill { pane_id } => {
                if let Err(e) = state.kill_pane(pane_id) {
                    send_reply(&sink, ServerMsg::Error(e.to_string()))
                } else {
                    // When: kill_pane succeeded, so no reply frame is owed; the pane's reader
                    // thread sends the Exit frame as it winds down.
                    Ok(())
                }
            }
            ClientMsg::Replay { pane_id } => {
                if let Err(e) = state.replay(pane_id, &sink) {
                    send_reply(&sink, ServerMsg::Error(e.to_string()))
                } else {
                    // When: every snapshot fragment queued, replay already resumed live delivery.
                    Ok(())
                }
            }
        };
        if let Err(error) = result {
            // When: a reply could not be delivered, so the transport is unusable; record the
            // error and leave the loop to run teardown once.
            request_error = Some(error);
            break;
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
    // `detach_subscriber` clears only (3) owned by this connection. We then
    // explicitly drop (1) and (2) before the `join` so the channel closes.
    state.detach_subscriber(&sink);
    drop(read_half);
    drop(tx);
    drop(sink);
    shutdown_writer();
    match writer_done_rx.recv_timeout(WRITER_SHUTDOWN_TIMEOUT) {
        Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
            if writer_thread.join().is_err() {
                tracing::warn!("mux client writer panicked during shutdown");
            }
        }
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
            tracing::warn!("mux client writer did not exit within the shutdown timeout");
            drop(writer_thread);
        }
    }
    request_error.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;
