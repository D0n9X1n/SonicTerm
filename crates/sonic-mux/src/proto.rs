//! Wire protocol for sonic-mux IPC.
//!
//! Length-prefixed bincode frames. Each frame is `u32` BE length followed by
//! the bincode-encoded payload.

use serde::{Deserialize, Serialize};

pub type SessionId = u64;
pub type PaneId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    ListSessions,
    /// Attach to an existing session by id. The server will reply with
    /// `AttachOk` and then begin streaming buffered + live output for every
    /// pane in that session.
    Attach(SessionId),
    Detach,
    /// Spawn a fresh pane in a (new or current) session. v0.1 places the new
    /// pane into a fresh session per spawn.
    Spawn {
        cmd: String,
        cols: u16,
        rows: u16,
    },
    Input {
        pane_id: PaneId,
        bytes: Vec<u8>,
    },
    Resize {
        pane_id: PaneId,
        cols: u16,
        rows: u16,
    },
    Kill {
        pane_id: PaneId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    Sessions(Vec<SessionInfo>),
    AttachOk {
        session_id: SessionId,
        panes: Vec<PaneInfo>,
    },
    /// New pane created (in response to Spawn).
    Spawned {
        session_id: SessionId,
        pane_id: PaneId,
    },
    Output {
        pane_id: PaneId,
        bytes: Vec<u8>,
    },
    Exit {
        pane_id: PaneId,
    },
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub pane_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: PaneId,
    pub cmd: String,
    pub cols: u16,
    pub rows: u16,
}
