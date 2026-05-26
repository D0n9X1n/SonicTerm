//! sonic-mux library surface: protocol types, framing, and server state.
//!
//! v0.1 ships:
//! - Process spawning + I/O echo over a local socket.
//! - Persistent PTY sessions across client disconnect/reattach.
//! - Per-pane replay buffer (256 KiB ring) flushed on Attach.
//!
//! FUTURE: full multi-pane sessions inside one Session, scrollback-aware
//! replay (not just a byte ring), multi-client fanout, auto-reconnect on
//! the client side, authentication on the socket.

pub mod frame;
pub mod proto;
pub mod server;

pub use proto::{ClientMsg, PaneId, PaneInfo, ServerMsg, SessionId, SessionInfo};
pub use server::{handle_connection, ServerState, SubscriberSink, CHANNEL_CAP, REPLAY_CAP};
