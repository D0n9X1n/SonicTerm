//! Cross-platform PTY spawning.
//!
//! Wraps the [`portable-pty`] crate so callers don't need to depend on it
//! directly. `PtyHandle` owns the slave-side child and the master read/write
//! pair, all decoupled by channels for use from the render thread.

#[cfg(target_os = "windows")]
use std::path::{Path, PathBuf};
use std::{
    io::{Read, Write},
    sync::Arc,
    thread,
};

use anyhow::Result;
use bytes::{Bytes, BytesMut};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

/// Outgoing message: bytes to write to the pty master (typed by user).
type Outgoing = Vec<u8>;
/// Incoming message: bytes read from the pty master (program output).
///
/// Uses [`bytes::Bytes`] — a refcounted slice — so the reader thread can
/// hand the buffer off to the VT thread without per-read `Vec::to_vec`
/// allocations. The reader keeps a single [`BytesMut`] ring of 64 KiB and
/// `split_to`s the filled prefix into a `Bytes` each iteration; once the
/// ring drains below capacity it reuses the same allocation.
type Incoming = Bytes;

/// Handle to a running pty process.
///
/// On drop, the child process is explicitly killed and the master writer is
/// dropped, which closes the pty fd and triggers EOF on the reader thread
/// so it exits cleanly. Without the explicit kill, dropping a `PtyHandle`
/// (e.g. on `Action::ClosePane`) would leave the shell as an orphan
/// connected to a closed pty until the OS reaps it.
pub struct PtyHandle {
    /// Channel of byte chunks read from the child's stdout/stderr.
    pub out_rx: Receiver<Incoming>,
    /// Channel for bytes / control messages to send to the child.
    pub in_tx: Sender<Outgoing>,
    /// Closure that resizes the pty to `(cols, rows)`.
    pub resize: Box<dyn Fn(u16, u16) + Send + Sync>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
}

impl PtyHandle {
    /// Explicitly terminate the child shell. Idempotent — second call is a
    /// no-op because the underlying handle will report it's already gone.
    /// Called automatically on Drop, but exposed for callers that want
    /// deterministic shutdown earlier.
    pub fn kill(&self) {
        let _ = self.child.lock().kill();
    }

    /// Process id of the underlying shell, if the platform reports it. Used
    /// by the tab-title renderer to probe the foreground process running in
    /// this pane's pty (e.g. "zsh" vs "nvim" vs "ssh"). Returns `None` if
    /// the OS layer doesn't expose a pid (rare) or if the child has already
    /// exited.
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().process_id()
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        // Only kill when this is the last live reference. Holding both halves
        // of `Arc` (e.g. for resize) is fine — the resize closure doesn't
        // outlive the handle in practice, but be defensive.
        if Arc::strong_count(&self.child) == 1 {
            self.kill();
        }
    }
}

impl PtyHandle {
    /// Spawn the user's default shell.
    pub fn spawn_default_shell(cols: u16, rows: u16) -> Result<Self> {
        let shell = default_shell();
        Self::spawn(&shell, cols, rows)
    }

    /// Spawn `cmd` (may include arguments via shell-style splitting handled
    /// upstream — we expect a single program path here for simplicity).
    pub fn spawn(cmd: &str, cols: u16, rows: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut builder = CommandBuilder::new(cmd);
        if let Ok(home) = std::env::var("HOME") {
            builder.cwd(home);
        }
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");

        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let master = pair.master;
        let reader = master.try_clone_reader()?;
        let writer = master.take_writer()?;
        let master = Arc::new(Mutex::new(master));

        let (out_tx, out_rx) = crossbeam_channel::unbounded::<Incoming>();
        let (in_tx, in_rx) = crossbeam_channel::unbounded::<Outgoing>();

        // Reader thread: pty -> out_rx.
        spawn_reader_thread(reader, out_tx);
        // Writer thread: in_rx -> pty.
        spawn_writer_thread(writer, in_rx);

        let resize_master = master.clone();
        let resize = Box::new(move |cols: u16, rows: u16| {
            let _ = resize_master.lock().resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        });

        Ok(Self { out_rx, in_tx, resize, child: Arc::new(Mutex::new(child)) })
    }

    /// Test-only constructor. Builds a `PtyHandle` whose `resize` invokes
    /// the caller-supplied closure (so tests can spy on resize calls) and
    /// whose underlying `Child` is a no-op stub (no real process spawned).
    ///
    /// `pub` + `#[doc(hidden)]` so integration tests in other crates can
    /// construct a `PtyHandle` without forking a real shell — needed by
    /// `sonicterm-app`'s per-pane-resize tests to assert `resize` is called
    /// on the survivor after `App::close_active_pane`. CLAUDE.md §5 bans
    /// `__test_support` shim modules, hence the doc-hidden public fn.
    #[doc(hidden)]
    pub fn for_test<F>(resize: F) -> Self
    where
        F: Fn(u16, u16) + Send + Sync + 'static,
    {
        let (_, out_rx) = crossbeam_channel::unbounded::<Incoming>();
        let (in_tx, _) = crossbeam_channel::unbounded::<Outgoing>();
        Self {
            out_rx,
            in_tx,
            resize: Box::new(resize),
            child: Arc::new(Mutex::new(Box::new(NoopChild) as Box<dyn Child + Send + Sync>)),
        }
    }
}

/// Test-only `Child` stub: implements the trait surface portable-pty needs
/// for `PtyHandle`'s `Drop` + `kill` paths to be no-ops. Exists only so
/// `PtyHandle::for_test` can construct a handle without spawning a real
/// process. Not exposed: lives behind `for_test`.
#[doc(hidden)]
#[derive(Debug)]
struct NoopChild;

impl portable_pty::ChildKiller for NoopChild {
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(NoopChild)
    }
}

impl portable_pty::Child for NoopChild {
    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        Ok(Some(portable_pty::ExitStatus::with_exit_code(0)))
    }
    fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        Ok(portable_pty::ExitStatus::with_exit_code(0))
    }
    fn process_id(&self) -> Option<u32> {
        None
    }
    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

fn spawn_reader_thread(mut reader: Box<dyn Read + Send>, tx: Sender<Incoming>) {
    thread::Builder::new()
        .name("sonic-pty-reader".into())
        .spawn(move || {
            // 64 KiB ring. We `split` the filled prefix into a `Bytes`
            // (refcounted view into the same allocation) on each read and
            // send it downstream. Once consumers drop their `Bytes`, the
            // next `reserve` call reclaims the original allocation in-place
            // — no per-read heap alloc, no `to_vec`. Replaces the previous
            // `[u8; 8192]` stack buffer + `buf[..n].to_vec()` pattern that
            // allocated once per read (and the reader can fire thousands of
            // reads per second under `cat largefile`).
            const RING_CAP: usize = 64 * 1024;
            // Keep at least one full PTY chunk (typical kernel pipe buffer
            // is 4–16 KiB) of headroom before each read to avoid forcing a
            // realloc mid-read.
            const READ_HEADROOM: usize = 8 * 1024;
            let mut buf = BytesMut::with_capacity(RING_CAP);
            loop {
                if buf.capacity() - buf.len() < READ_HEADROOM {
                    // If downstream has dropped its `Bytes` views, this
                    // reclaims the original buffer; otherwise it allocates
                    // a fresh one and drops our half of the previous ring.
                    buf.reserve(RING_CAP);
                }
                // Zero-initialise the spare region before handing it to
                // `Read::read`. `Read` requires an initialised destination
                // slice (passing `MaybeUninit` bytes via a `&mut [u8]` cast
                // is UB even though most impls never read from it). The
                // memset cost on a 64 KiB region is dominated by the syscall
                // itself; the underlying allocation is still reused across
                // reads, preserving the zero-alloc steady state.
                let initial_len = buf.len();
                let read_cap = buf.capacity() - initial_len;
                buf.resize(initial_len + read_cap, 0);
                match reader.read(&mut buf[initial_len..]) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.truncate(initial_len + n);
                        let chunk = buf.split().freeze();
                        if tx.send(chunk).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("pty read error: {e}");
                        break;
                    }
                }
            }
        })
        // PANIC: thread::Builder::spawn only fails on OS-level resource
        // exhaustion (out of memory / out of process handles). At terminal
        // startup we cannot meaningfully recover — propagating a Result up
        // through `spawn_pane` would land on the same `expect`. Documented.
        .expect("spawn pty reader");
}

fn spawn_writer_thread(mut writer: Box<dyn Write + Send>, rx: Receiver<Outgoing>) {
    thread::Builder::new()
        .name("sonic-pty-writer".into())
        .spawn(move || {
            while let Ok(bytes) = rx.recv() {
                if let Err(e) = writer.write_all(&bytes) {
                    tracing::warn!("pty write error: {e}");
                    break;
                }
                let _ = writer.flush();
            }
        })
        // PANIC: see `spawn_reader_thread` rationale above — OS-level
        // thread-spawn failure at PTY init is unrecoverable.
        .expect("spawn pty writer");
}

fn default_shell() -> String {
    default_shell_program()
}

#[cfg(target_os = "windows")]
fn default_shell_program() -> String {
    path_lookup("pwsh.exe")
        .or_else(|| path_lookup("powershell.exe"))
        .unwrap_or_else(|| "cmd.exe".to_string())
}

#[cfg(not(target_os = "windows"))]
fn default_shell_program() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}

#[cfg(target_os = "windows")]
fn path_lookup(name: &str) -> Option<String> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate.to_string_lossy().to_string());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir: PathBuf| dir.join(name))
        .find(|candidate| candidate.is_file())
        .map(|path| path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::default_shell_program;

    #[test]
    fn default_shell_program_returns_platform_default() {
        let shell = default_shell_program();
        #[cfg(target_os = "windows")]
        {
            let lower = shell.to_ascii_lowercase();
            assert!(
                lower.ends_with("pwsh.exe")
                    || lower.ends_with("powershell.exe")
                    || lower == "cmd.exe",
                "unexpected default shell: {shell}"
            );
        }
        #[cfg(not(target_os = "windows"))]
        assert!(!shell.is_empty());
    }
}
