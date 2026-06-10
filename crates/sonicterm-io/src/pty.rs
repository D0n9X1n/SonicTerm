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
#[derive(Clone, Debug)]
pub struct ShellSpawnOpts {
    /// Suppress shell startup banner/profile and emit clean-mode args
    /// (PowerShell `-NoLogo -NoProfile`, bash `--norc --noprofile`,
    /// zsh `-f`). For e2e gates only — production app keeps default.
    pub clean_e2e: bool,
    /// `TERM_PROGRAM` value injected into the child PTY environment.
    /// Defaults to `SonicTerm` to preserve existing terminal identity.
    pub term_program: String,
}

impl ShellSpawnOpts {
    /// Production default `TERM_PROGRAM` value.
    pub const DEFAULT_TERM_PROGRAM: &'static str = "SonicTerm";
}

impl Default for ShellSpawnOpts {
    fn default() -> Self {
        Self { clean_e2e: false, term_program: Self::DEFAULT_TERM_PROGRAM.to_string() }
    }
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

    /// Resolved shell program path (the command we actually spawned).
    pub fn shell_program_path(&self) -> &str {
        &self.shell_program_path
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        // LM-007 (#598): the previous `Arc::strong_count == 1` guard skipped
        // the kill in any code path that still held a cloned child Arc,
        // leaving an orphaned shell on tab close. It also relied on
        // portable-pty's `Child::kill`, which on Unix sends SIGHUP and only
        // escalates to SIGKILL after a timing window. Shells that trap
        // SIGHUP (zsh by default, bash with `trap '' HUP`) survive that
        // window and outlive the PTY as orphans.
        //
        // Fix: always kill — send SIGKILL directly via `libc::kill` first,
        // then call portable-pty's `kill()` (covers any pid-namespace edge
        // cases and the Windows `TerminateProcess` path), then reap with
        // a bounded `try_wait` poll so a stuck child can never hang the
        // app on tab close.
        let mut child = self.child.lock();
        #[cfg(unix)]
        let pid_for_log = child.process_id();
        #[cfg(unix)]
        if let Some(pid) = child.process_id() {
            // SAFETY: libc::kill is FFI; pid comes from portable-pty's
            // tracked child handle. ESRCH (already dead) is fine.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        // portable-pty escalation as defense in depth (Windows
        // TerminateProcess, Unix pid-namespace edge cases).
        let _ = child.kill();
        // Bounded reap. After SIGKILL the kernel typically delivers the
        // exit status in well under 10ms; the 500ms budget is huge
        // headroom for pathological scheduling. We must never block the
        // app indefinitely on tab close: if the child somehow doesn't
        // reap (kernel bug, exotic process-group state) we log and move
        // on rather than hang Drop.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    #[cfg(unix)]
                    {
                        tracing::warn!(
                            pid = pid_for_log,
                            "PtyHandle::Drop: child did not exit within 500ms after SIGKILL"
                        );
                    }
                    #[cfg(not(unix))]
                    {
                        tracing::warn!(
                            "PtyHandle::Drop: child did not exit within 500ms after kill"
                        );
                    }
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(_) => break,
            }
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
        let args = shell_startup_args(&shell, opts.clone());
        Self::spawn_with_args_and_opts(&shell, &args, cols, rows, opts)
    }

    /// Spawn `cmd` (may include arguments via shell-style splitting handled
    /// upstream — we expect a single program path here for simplicity).
    pub fn spawn(cmd: &str, cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with_args(cmd, &[], cols, rows)
    }

    /// Internal: spawn `cmd` with `args`. The public `spawn` + `spawn_default_shell`
    /// converge here so opts-derived args (e.g. `-NoLogo -NoProfile` for
    /// PowerShell clean_e2e) reach `CommandBuilder` consistently.
    ///
    /// Also `pub` (doc-hidden) so integration tests can spawn shells with
    /// args (e.g. `bash -c "trap '' HUP; exec cat"` for the #598 LM-007
    /// regression test) without re-implementing the whole pipeline.
    #[doc(hidden)]
    pub fn spawn_with_args(cmd: &str, args: &[String], cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with_args_and_opts(cmd, args, cols, rows, ShellSpawnOpts::default())
    }

    /// Internal: spawn `cmd` with `args` and explicit environment options.
    #[doc(hidden)]
    pub fn spawn_with_args_and_opts(
        cmd: &str,
        args: &[String],
        cols: u16,
        rows: u16,
        opts: ShellSpawnOpts,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut builder = CommandBuilder::new(cmd);
        for a in args {
            builder.arg(a);
        }
        if let Ok(home) = std::env::var("HOME") {
            builder.cwd(home);
        }
        apply_child_pty_env(&mut builder, &opts.term_program);

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

const DEFAULT_LANG_UTF8_LOCALE: &str = "en_US.UTF-8";
const DEFAULT_LC_CTYPE_UTF8_LOCALE: &str = "UTF-8";

/// Return startup arguments for the selected shell.
///
/// Production macOS shells are login shells so `/etc/zprofile` can run
/// `path_helper`, matching Terminal.app/iTerm2/WezTerm PATH behavior. Clean
/// E2E mode intentionally bypasses profiles for deterministic fixtures.
#[doc(hidden)]
pub fn shell_startup_args(shell_path: &str, opts: ShellSpawnOpts) -> Vec<String> {
    if opts.clean_e2e {
        clean_e2e_args(shell_path)
    } else {
        interactive_shell_args(shell_path)
    }
}

#[cfg(target_os = "macos")]
fn apply_terminal_locale_env(builder: &mut CommandBuilder) {
    let lc_all = builder.get_env("LC_ALL").and_then(|v| v.to_str());
    let lc_ctype = builder.get_env("LC_CTYPE").and_then(|v| v.to_str());
    let lang = builder.get_env("LANG").and_then(|v| v.to_str());

    if should_apply_utf8_locale_fallback(lc_all, lc_ctype, lang) {
        if is_empty_env(lang) {
            builder.env("LANG", DEFAULT_LANG_UTF8_LOCALE);
        }
        builder.env("LC_CTYPE", DEFAULT_LC_CTYPE_UTF8_LOCALE);
    }
}

#[cfg(not(target_os = "macos"))]
fn apply_terminal_locale_env(_builder: &mut CommandBuilder) {}

#[doc(hidden)]
pub fn should_apply_utf8_locale_fallback(
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
    lang: Option<&str>,
) -> bool {
    if !is_empty_env(lc_all) {
        return false;
    }
    !is_utf8_locale(lc_ctype) && !is_utf8_locale(lang)
}

#[doc(hidden)]
pub const fn default_lang_utf8_locale() -> &'static str {
    DEFAULT_LANG_UTF8_LOCALE
}

#[doc(hidden)]
pub const fn default_lc_ctype_utf8_locale() -> &'static str {
    DEFAULT_LC_CTYPE_UTF8_LOCALE
}

fn is_empty_env(value: Option<&str>) -> bool {
    value.map(str::trim).unwrap_or_default().is_empty()
}

fn is_utf8_locale(value: Option<&str>) -> bool {
    let Some(value) = value else { return false };
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    normalized.contains("utf-8") || normalized.contains("utf8")
}

#[cfg(target_os = "macos")]
fn interactive_shell_args(shell_path: &str) -> Vec<String> {
    let name = shell_file_name(shell_path);
    match name.as_str() {
        "zsh" | "zsh.exe" | "tcsh" | "csh" => vec!["-l".to_string()],
        "bash" | "bash.exe" | "fish" | "fish.exe" => vec!["--login".to_string()],
        _ => Vec::new(),
    }
}

#[cfg(not(target_os = "macos"))]
fn interactive_shell_args(_shell_path: &str) -> Vec<String> {
    Vec::new()
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
    let name = shell_file_name(shell_path);
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

#[doc(hidden)]
pub fn apply_child_pty_env(builder: &mut CommandBuilder, term_program: &str) {
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    // Identify the terminal to programs that branch on TERM_PROGRAM
    // (e.g. Copilot CLI, shells, prompt frameworks). Mirrors iTerm2 /
    // WezTerm, which set TERM_PROGRAM + TERM_PROGRAM_VERSION.
    builder.env("TERM_PROGRAM", term_program);
    builder.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    apply_terminal_locale_env(builder);
}

fn shell_file_name(shell_path: &str) -> String {
    Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_str<'a>(builder: &'a CommandBuilder, name: &str) -> &'a str {
        builder.get_env(name).and_then(|v| v.to_str()).unwrap()
    }

    #[test]
    fn default_shell_spawn_opts_keep_sonicterm_term_program() {
        let opts = ShellSpawnOpts::default();
        assert_eq!(opts.term_program, ShellSpawnOpts::DEFAULT_TERM_PROGRAM);
    }

    #[test]
    fn child_pty_env_uses_configured_term_program() {
        let mut builder = CommandBuilder::new("sh");
        apply_child_pty_env(&mut builder, "WezTerm");

        assert_eq!(env_str(&builder, "TERM"), "xterm-256color");
        assert_eq!(env_str(&builder, "COLORTERM"), "truecolor");
        assert_eq!(env_str(&builder, "TERM_PROGRAM"), "WezTerm");
        assert_eq!(env_str(&builder, "TERM_PROGRAM_VERSION"), env!("CARGO_PKG_VERSION"));
    }
}
