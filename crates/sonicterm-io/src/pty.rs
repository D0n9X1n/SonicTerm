//! Cross-platform PTY spawning.
//!
//! Wraps the [`portable-pty`] crate so callers don't need to depend on it
//! directly. `PtyHandle` owns the slave-side child and the master read/write
//! pair, all decoupled by channels for use from the render thread.

use std::path::Path;
#[cfg(target_os = "windows")]
use std::path::PathBuf;
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
    /// Resolved shell program path (the command we actually spawned).
    /// Used by the e2e gates to pick the right `ShellDialect`. For
    /// `for_test`, this is a sentinel `"<sonicterm-pty-test-sentinel>"`.
    shell_program_path: String,
}

/// Options controlling how `spawn_default_shell` constructs the shell
/// command line. Default is interactive behavior (preserve user profile,
/// banner, prompt). E2E gates / examples that need deterministic output
/// pass `clean_e2e: true` to suppress profile/logo and emit shell-family-
/// specific clean-startup args.
///
/// Added per #457 — pre-PR examples sent POSIX `printf` to PowerShell,
/// producing zero output. PLAN v5 split the fix into:
///   1. (this) — add opts + WindowsApps stub filter + shell-path accessor
///   2. (next PR) — ShellDialect trait + golden fixtures + actual e2e fix
#[derive(Clone, Debug, Default)]
pub struct ShellSpawnOpts {
    /// Suppress shell startup banner/profile and emit clean-mode args
    /// (PowerShell `-NoLogo -NoProfile`, bash `--norc --noprofile`,
    /// zsh `-f`). For e2e gates only — production app keeps default.
    pub clean_e2e: bool,
}

/// Sentinel value `PtyHandle::shell_program_path` returns for the test-only
/// constructor `for_test`. `dialect_for_shell` (in `sonicterm-core::test_support::shell_dialect`,
/// follow-up PR) explicitly rejects it so test fixtures fail loud if
/// misused as a real shell.
pub const TEST_SENTINEL_SHELL_PATH: &str = "<sonicterm-pty-test-sentinel>";

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

    /// Resolved shell program path (the command we actually spawned).
    /// For `for_test`, returns the sentinel `<sonicterm-pty-test-sentinel>`.
    ///
    /// Added per #457 so the e2e gates (`pty_dump`, `pty_dump_unicode`)
    /// can pick the right `ShellDialect` for the shell that was actually
    /// resolved (pwsh, powershell, bash, zsh, etc.).
    pub fn shell_program_path(&self) -> &str {
        &self.shell_program_path
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
    ///
    /// `opts.clean_e2e=true` suppresses shell startup banner/profile and
    /// emits clean-mode args (PowerShell `-NoLogo -NoProfile`, bash
    /// `--norc --noprofile`, zsh `-f`). E2E gates pass `true`; the
    /// production app passes `ShellSpawnOpts::default()` to preserve
    /// interactive behavior.
    pub fn spawn_default_shell(cols: u16, rows: u16, opts: ShellSpawnOpts) -> Result<Self> {
        let shell = default_shell();
        let args = if opts.clean_e2e { clean_e2e_args(&shell) } else { Vec::new() };
        Self::spawn_with_args(&shell, &args, cols, rows)
    }

    /// Spawn `cmd` (may include arguments via shell-style splitting handled
    /// upstream — we expect a single program path here for simplicity).
    pub fn spawn(cmd: &str, cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with_args(cmd, &[], cols, rows)
    }

    /// Internal: spawn `cmd` with `args`. The public `spawn` + `spawn_default_shell`
    /// converge here so opts-derived args (e.g. `-NoLogo -NoProfile` for
    /// PowerShell clean_e2e) reach `CommandBuilder` consistently.
    fn spawn_with_args(cmd: &str, args: &[String], cols: u16, rows: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut builder = CommandBuilder::new(cmd);
        for a in args {
            builder.arg(a);
        }
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

        Ok(Self {
            out_rx,
            in_tx,
            resize,
            child: Arc::new(Mutex::new(child)),
            shell_program_path: cmd.to_string(),
        })
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
            shell_program_path: TEST_SENTINEL_SHELL_PATH.to_string(),
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
    let allow_windowsapps =
        std::env::var("SONICTERM_ALLOW_WINDOWSAPPS_SHELL").map(|v| v == "1").unwrap_or(false);
    std::env::split_paths(&path)
        .map(|dir: PathBuf| dir.join(name))
        .find(|candidate| {
            if !candidate.is_file() {
                return false;
            }
            // #457: skip Microsoft Store WindowsApps stubs for `pwsh.exe` /
            // `powershell.exe`. The App Execution Alias produces zero output
            // under ConPTY when spawned bare, so the e2e gates silently fail.
            // Escape hatch: SONICTERM_ALLOW_WINDOWSAPPS_SHELL=1 to opt back in.
            let lname = name.to_ascii_lowercase();
            let is_powershell = lname.ends_with("pwsh.exe") || lname.ends_with("powershell.exe");
            if is_powershell && !allow_windowsapps {
                let lpath = candidate.to_string_lossy().to_lowercase();
                if lpath.contains("\\windowsapps\\") {
                    return false;
                }
            }
            true
        })
        .map(|path| path.to_string_lossy().to_string())
}

/// Returns clean-startup args appropriate for the resolved shell. For
/// PowerShell (`pwsh.exe` / `powershell.exe`), emits `-NoLogo -NoProfile`.
/// For bash, emits `--norc --noprofile`. For zsh, emits `-f` (skips
/// `.zshrc` but NOT `.zshenv` — `.zshenv` is for required env setup,
/// and replacing `-f` with `--no-rcs` would be a behavior change rather
/// than a fix). Unknown shells get no args.
///
/// Used only when `ShellSpawnOpts::clean_e2e = true`.
pub(crate) fn clean_e2e_args(shell_path: &str) -> Vec<String> {
    let name = Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match name.as_str() {
        "pwsh.exe" | "powershell.exe" | "pwsh" | "powershell" => {
            vec!["-NoLogo".to_string(), "-NoProfile".to_string()]
        }
        "bash" | "bash.exe" => {
            vec!["--norc".to_string(), "--noprofile".to_string()]
        }
        "zsh" | "zsh.exe" => {
            vec!["-f".to_string()]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // #457 clean_e2e_args coverage

    #[test]
    fn clean_e2e_args_powershell_returns_nologo_noprofile() {
        // `mut` only needed on Windows where we extend with absolute paths.
        #[cfg(target_os = "windows")]
        let mut shells: Vec<&str> = vec!["pwsh.exe", "powershell.exe"];
        #[cfg(not(target_os = "windows"))]
        let shells: Vec<&str> = vec!["pwsh.exe", "powershell.exe"];
        // Windows-style absolute paths only parse via Path::new on Windows
        // (Unix Path treats '\\' as literal characters, not separators).
        #[cfg(target_os = "windows")]
        shells.extend([
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
        ]);
        for shell in shells {
            assert_eq!(clean_e2e_args(shell), vec!["-NoLogo", "-NoProfile"], "shell={shell}");
        }
    }

    #[test]
    fn clean_e2e_args_bash_returns_norc_noprofile() {
        assert_eq!(clean_e2e_args("bash"), vec!["--norc", "--noprofile"]);
        assert_eq!(clean_e2e_args("/bin/bash"), vec!["--norc", "--noprofile"]);
        assert_eq!(clean_e2e_args("bash.exe"), vec!["--norc", "--noprofile"]);
    }

    #[test]
    fn clean_e2e_args_zsh_returns_dash_f() {
        // -f skips .zshrc/.zlogin/.zlogout but intentionally NOT .zshenv
        // (required env setup). Switching to --no-rcs would change behavior.
        assert_eq!(clean_e2e_args("zsh"), vec!["-f"]);
        assert_eq!(clean_e2e_args("/bin/zsh"), vec!["-f"]);
        assert_eq!(clean_e2e_args("zsh.exe"), vec!["-f"]);
    }

    #[test]
    fn clean_e2e_args_unknown_shell_returns_empty() {
        // Unknown shells get no args — passing unsupported flags can
        // prevent shell startup entirely.
        assert!(clean_e2e_args("cmd.exe").is_empty());
        assert!(clean_e2e_args("fish").is_empty());
        assert!(clean_e2e_args("nu").is_empty());
        assert!(clean_e2e_args("").is_empty());
    }

    #[test]
    fn shell_spawn_opts_default_is_interactive() {
        let opts = ShellSpawnOpts::default();
        assert!(!opts.clean_e2e, "Default opts must preserve interactive shell behavior");
    }

    #[test]
    fn for_test_handle_returns_sentinel_shell_path() {
        let h = PtyHandle::for_test(|_, _| {});
        assert_eq!(h.shell_program_path(), TEST_SENTINEL_SHELL_PATH);
    }
}
