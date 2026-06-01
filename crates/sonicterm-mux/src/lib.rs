//! sonicterm-mux library surface: protocol types, framing, and server state.
//!
//! v0.1 ships:
//! - Process spawning + I/O echo over a local socket.
//! - Persistent PTY sessions across client disconnect/reattach.
//! - Per-pane replay buffer (256 KiB ring) flushed on Attach.
//!
//! FUTURE: full multi-pane sessions inside one Session, scrollback-aware
//! replay (not just a byte ring), multi-client fanout, auto-reconnect on
//! the client side, authentication on the socket.

#![deny(missing_docs)]

/// Length-prefixed JSON framing over the mux socket (`read_frame`/`write_frame`).
pub mod frame;
/// Wire types — `ClientMsg`, `ServerMsg`, and the id/info structs they carry.
pub mod proto;
/// Server-side state machine: sessions, panes, replay rings, subscriber fanout.
pub mod server;

pub use proto::{ClientMsg, PaneId, PaneInfo, ServerMsg, SessionId, SessionInfo};
pub use server::{handle_connection, ServerState, SubscriberSink, CHANNEL_CAP, REPLAY_CAP};
