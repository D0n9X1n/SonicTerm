//! PTY backend trait. Implementers: `sonicterm-io::pty::PtyHandle` (mac
//! `portable-pty`, win `conpty`).
//!
//! Must be **object-safe** (consumers store `Box<dyn PtyTransport>`).
//! Implementers **must** kill the child in `Drop` (see LM-007).

use std::io;

/// Minimal PTY abstraction over read/write/resize/wait.
///
/// All methods are blocking unless explicitly noted. The caller is
/// expected to run read/write on a dedicated thread (see
/// `sonicterm-app::spawn_pane`).
pub trait PtyTransport: Send {
    /// Read pending bytes from the PTY master into `buf`. Returns the
    /// number of bytes read, or `0` on EOF.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Write `buf` to the PTY master.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;

    /// Resize the PTY to `cols` × `rows`. Must propagate to the child
    /// process (`TIOCSWINSZ` on unix, `ResizePseudoConsole` on win).
    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()>;

    /// Best-effort wait for child exit. Returns `None` if still running.
    fn try_wait(&mut self) -> io::Result<Option<i32>>;
}
