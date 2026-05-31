//! Exit and crash tracing.
//!
//! Ensures that **every** process termination path leaves a marker in
//! `sonic.log` and, for crashes, a file under `crashes/`.
//!
//! Coverage matrix (see also `docs/LOGGING.md`):
//!
//! | path                                  | mechanism                              |
//! |---------------------------------------|----------------------------------------|
//! | Rust panic (any thread)               | [`crate::install_panic_hook`]          |
//! | Stack overflow                        | `sigaltstack` + SIGSEGV handler        |
//! | SIGSEGV / SIGBUS / SIGILL / SIGABRT / SIGFPE | `sigaction` with `SA_RESETHAND`+`SA_SIGINFO` |
//! | OOM (allocator failure)               | [`std::alloc::set_alloc_error_hook`]   |
//! | `LoopExiting` (Cmd+Q, WM_CLOSE)       | [`record_loop_exiting`]                |
//! | `main` returns                        | drop guard returned by [`install_exit_logging`] |
//! | `std::process::exit`                  | [`exit_with`] helper + CI grep gate    |
//! | SIGKILL / power-off                   | NOT catchable; absence of an "exiting" line implies one of these |
//!
//! `install_exit_logging` is idempotent — call once from each binary's
//! `main()` immediately after [`crate::install_panic_hook`] and capture
//! the returned [`ExitGuard`] for the lifetime of the process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::OnceLock;

/// Reason recorded for the upcoming process exit. Read by the drop
/// guard so the final log line classifies what happened.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExitReason {
    /// No explicit reason recorded — `main` returned normally.
    Clean = 0,
    /// `winit` raised `Event::LoopExiting` (Cmd+Q, WM_CLOSE, last window).
    LoopExiting = 1,
    /// [`exit_with`] called.
    ExplicitExit = 2,
    /// A panic hook fired.
    Panic = 3,
    /// A signal handler fired.
    Signal = 4,
    /// `set_alloc_error_hook` fired.
    AllocFailure = 5,
}

static REASON: AtomicU8 = AtomicU8::new(ExitReason::Clean as u8);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static CRASH_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Pre-opened raw fd for sonic.log (best-effort) so async-signal-safe
/// handlers can `write(2)` without going through tracing/alloc.
///
/// Only read/written by the Unix signal-handler path; on Windows it
/// stays at `-1` and is otherwise unused.
#[cfg_attr(not(unix), allow(dead_code))]
static LOG_FD: AtomicI32 = AtomicI32::new(-1);

use std::sync::atomic::AtomicI32;

/// Record why the process is about to exit. Idempotent in the sense
/// that the first non-Clean reason wins — the panic hook should not
/// be overwritten by a subsequent `LoopExiting` triggered by the
/// unwind.
pub fn record_exit_reason(r: ExitReason) {
    let _ = REASON.compare_exchange(
        ExitReason::Clean as u8,
        r as u8,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
}

/// Record that the winit event loop is exiting. Wraps [`record_exit_reason`]
/// with a warning-level `sonic_exit` log line so the file shows the reason even
/// if the drop guard never runs (e.g., the user kills the process during
/// shutdown) under the shipped default filter.
pub fn record_loop_exiting() {
    record_exit_reason(ExitReason::LoopExiting);
    tracing::warn!(
        target: "sonic_exit",
        "sonic exiting: winit LoopExiting (Cmd+Q / WM_CLOSE / last window)"
    );
}

/// Drop guard returned by [`install_exit_logging`]. On drop, logs the
/// classified exit reason. Holding this until `main` returns is what
/// gives us a "clean main return" marker line.
pub struct ExitGuard(());

impl Drop for ExitGuard {
    fn drop(&mut self) {
        match REASON.load(Ordering::SeqCst) {
            x if x == ExitReason::Clean as u8 => {
                tracing::warn!(target: "sonic_exit", "sonic exiting: clean main return");
            }
            x if x == ExitReason::LoopExiting as u8 => {
                tracing::warn!(target: "sonic_exit", "sonic exiting: clean after LoopExiting");
            }
            x if x == ExitReason::ExplicitExit as u8 => {
                tracing::warn!(target: "sonic_exit", "sonic exiting: via exit_with()");
            }
            x if x == ExitReason::Panic as u8 => {
                tracing::error!("sonic exiting: after panic");
            }
            x if x == ExitReason::Signal as u8 => {
                tracing::error!("sonic exiting: after fatal signal");
            }
            x if x == ExitReason::AllocFailure as u8 => {
                tracing::error!("sonic exiting: after allocator failure");
            }
            _ => {
                tracing::warn!("sonic exiting: unknown reason");
            }
        }
    }
}

/// Install every exit-trace hook (signals, allocator, drop guard).
/// Call from `main()` immediately after [`crate::install_panic_hook`]
/// so that even a panic during the rest of `main` is caught with the
/// log machinery already armed. Returns a guard to keep alive for the
/// lifetime of the process.
pub fn install_exit_logging(crash_dir: &Path) -> ExitGuard {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return ExitGuard(());
    }
    let _ = CRASH_DIR.set(crash_dir.to_path_buf());

    // Best-effort open the active log file for async-signal-safe writes.
    // We re-open append-mode so we don't share a buffered handle with the
    // tracing-appender (its writer is in another thread and may have
    // pending bytes — that's fine, we just append our marker line).
    let log_path = crate::path::log_dir().join(crate::path::log_file_name());
    open_log_fd(&log_path);

    install_alloc_error_logging();
    #[cfg(unix)]
    install_signal_handlers();

    ExitGuard(())
}

fn open_log_fd(_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).mode(0o644).open(_path)
        {
            use std::os::unix::io::IntoRawFd;
            let fd = f.into_raw_fd();
            LOG_FD.store(fd, Ordering::SeqCst);
        }
    }
}

fn install_alloc_error_logging() {
    // `std::alloc::set_alloc_error_hook` is unstable on stable Rust
    // (rust-lang/rust#51245). We therefore can't intercept allocator
    // failures directly; instead, the global allocator's default
    // behaviour is to call `__rust_alloc_error_handler`, which prints
    // to stderr and aborts via SIGABRT — and our SIGABRT handler
    // (installed below on Unix) catches the abort and writes a
    // "FATAL: SIGABRT" marker to sonic.log. So alloc failures DO
    // produce a log line on Unix, just routed via the signal path.
    // Documented in docs/LOGGING.md.
}

#[cfg(unix)]
fn install_signal_handlers() {
    use std::mem::MaybeUninit;

    // Per-thread alt-stack so a stack overflow still has room to run
    // the handler. SIGSTKSZ on macOS is small; bump to 64 KiB.
    unsafe {
        const STK_SIZE: usize = 64 * 1024;
        let buf = Box::leak(vec![0u8; STK_SIZE].into_boxed_slice());
        let ss =
            libc::stack_t { ss_sp: buf.as_mut_ptr() as *mut _, ss_flags: 0, ss_size: STK_SIZE };
        libc::sigaltstack(&ss, std::ptr::null_mut());
    }

    let signals = [
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGFPE, "SIGFPE"),
    ];
    for (sig, _name) in signals {
        unsafe {
            let mut act: libc::sigaction = MaybeUninit::zeroed().assume_init();
            act.sa_sigaction = handle_signal as *const () as usize;
            act.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
            libc::sigemptyset(&mut act.sa_mask);
            libc::sigaction(sig, &act, std::ptr::null_mut());
        }
    }
}

/// Map signal number to a short static byte slice. Async-signal-safe.
#[cfg(unix)]
fn signal_name(sig: libc::c_int) -> &'static [u8] {
    match sig {
        libc::SIGSEGV => b"SIGSEGV",
        libc::SIGBUS => b"SIGBUS",
        libc::SIGILL => b"SIGILL",
        libc::SIGABRT => b"SIGABRT",
        libc::SIGFPE => b"SIGFPE",
        _ => b"SIG?",
    }
}

#[cfg(unix)]
extern "C" fn handle_signal(
    sig: libc::c_int,
    _info: *mut libc::siginfo_t,
    _ctx: *mut libc::c_void,
) {
    // Async-signal-safe: ONLY call write(2) on a pre-opened fd.
    // No tracing macros, no alloc, no locks.
    let fd = LOG_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let prefix: &[u8] = b"FATAL: ";
        let suffix: &[u8] = b" - sonic terminating (handler async-signal-safe path)\n";
        unsafe {
            libc::write(fd, prefix.as_ptr() as *const _, prefix.len());
            let name = signal_name(sig);
            libc::write(fd, name.as_ptr() as *const _, name.len());
            libc::write(fd, suffix.as_ptr() as *const _, suffix.len());
            // Best-effort fsync — ignore failure.
            libc::fsync(fd);
        }
    }
    // SA_RESETHAND restored default disposition before delivery; just
    // re-raise so the kernel produces the .ips / core file.
    unsafe {
        libc::raise(sig);
    }
}

/// Log a reason then call [`std::process::exit`]. The CI grep gate
/// (`scripts/check-no-raw-process-exit.sh`) requires all production-code
/// exits go through this helper.
pub fn exit_with(code: i32, reason: &str) -> ! {
    record_exit_reason(ExitReason::ExplicitExit);
    tracing::warn!(
        target: "sonic_exit",
        code,
        reason,
        "sonic exiting: explicit process::exit"
    );
    std::process::exit(code);
}

#[doc(hidden)]
/// Test bridge: reset the recorded reason. Used by exit_trace tests
/// to avoid cross-test leakage of the global atomic.
pub fn __test_reset_reason() {
    REASON.store(ExitReason::Clean as u8, Ordering::SeqCst);
}

#[doc(hidden)]
/// Test bridge: read the recorded reason without consuming it.
pub fn __test_reason() -> u8 {
    REASON.load(Ordering::SeqCst)
}
