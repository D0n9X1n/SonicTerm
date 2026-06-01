//! Wire protocol for sonicterm-mux IPC.
//!
//! Length-prefixed bincode frames. Each frame is `u32` BE length followed by
//! the bincode-encoded payload.

use serde::{Deserialize, Serialize};

/// Server-assigned, monotonically allocated identifier for a multiplexed session.
pub type SessionId = u64;
/// Server-assigned, monotonically allocated identifier for a pane within a session.
pub type PaneId = u64;

/// Messages sent from a client (e.g. the SonicTerm GUI) to the mux server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    /// Ask the server to enumerate all live sessions; replied with `Sessions`.
    ListSessions,
    /// Attach to an existing session by id. The server will reply with
    /// `AttachOk` and then begin streaming buffered + live output for every
    /// pane in that session.
    Attach(SessionId),
    /// Detach the current connection from its session — sessions outlive
    /// disconnects so the client can reattach later.
    Detach,
    /// Spawn a fresh pane in a (new or current) session. v0.1 places the new
    /// pane into a fresh session per spawn.
    Spawn {
        /// Shell or program to exec (e.g. `/bin/zsh`).
        cmd: String,
        /// Initial PTY column count.
        cols: u16,
        /// Initial PTY row count.
        rows: u16,
    },
    /// Forward keystrokes / paste bytes to a pane's PTY stdin.
    Input {
        /// Target pane.
        pane_id: PaneId,
        /// Raw bytes to write to the pane's master fd.
        bytes: Vec<u8>,
    },
    /// Notify the server the client resized; server propagates via `TIOCSWINSZ`.
    Resize {
        /// Target pane.
        pane_id: PaneId,
        /// New column count.
        cols: u16,
        /// New row count.
        rows: u16,
    },
    /// Terminate a pane (SIGKILL its child + drop server-side state).
    Kill {
        /// Target pane.
        pane_id: PaneId,
    },
}

/// Messages the server pushes to a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Reply to `ListSessions` — snapshot of currently-live sessions.
    Sessions(Vec<SessionInfo>),
    /// Reply to `Attach` — confirms the bind and lists panes the client should
    /// expect output for.
    AttachOk {
        /// Session the client is now attached to.
        session_id: SessionId,
        /// All panes currently live in that session.
        panes: Vec<PaneInfo>,
    },
    /// New pane created (in response to Spawn).
    Spawned {
        /// Session the new pane joined (may be freshly minted).
        session_id: SessionId,
        /// Newly allocated pane id.
        pane_id: PaneId,
    },
    /// PTY output for a pane — either replay-buffer bytes flushed on attach or
    /// fresh live bytes from the child process.
    Output {
        /// Originating pane.
        pane_id: PaneId,
        /// Raw bytes read from the pane's master fd.
        bytes: Vec<u8>,
    },
    /// Pane's child process exited; the server has already cleaned up its slot.
    Exit {
        /// Pane that just exited.
        pane_id: PaneId,
    },
    /// Out-of-band protocol error (bad request, internal failure). Free-form text.
    Error(String),
}

/// Summary returned for each live session in `ServerMsg::Sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session id (stable for the lifetime of the session).
    pub id: SessionId,
    /// Number of panes currently alive in the session.
    pub pane_count: usize,
}

/// Per-pane metadata returned in `ServerMsg::AttachOk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Pane id, unique within and across sessions for the server's lifetime.
    pub id: PaneId,
    /// Command line the pane is running (purely informational).
    pub cmd: String,
    /// Current PTY column count.
    pub cols: u16,
    /// Current PTY row count.
    pub rows: u16,
}
