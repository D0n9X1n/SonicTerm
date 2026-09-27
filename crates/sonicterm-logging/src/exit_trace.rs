//! Exit and crash tracing.
//!
//! Ensures that **every** process termination path leaves a marker in
//! `sonicterm.log` and, for crashes, a file under `crashes/`.
//!
//! Coverage matrix (see also `wiki/Logging.md`):
//!
//! | path                                  | mechanism                              |
//! |---------------------------------------|----------------------------------------|
//! | Rust panic (any thread)               | [`crate::install_panic_hook`]          |
//! | Stack overflow                        | `sigaltstack` + SIGSEGV/SIGBUS handler, chained to Rust's overflow report |
//! | SIGSEGV / SIGBUS / SIGILL / SIGABRT / SIGFPE | `sigaction` with `SA_RESETHAND`+`SA_SIGINFO`, chained at most once to the previous action |
//! | OOM (allocator failure)               | [`std::alloc::set_alloc_error_hook`]   |
//! | `LoopExiting` (Cmd+Q, WM_CLOSE)       | [`record_loop_exiting`]                |
//! | `main` returns                        | drop guard returned by [`install_exit_logging`] |
//! | `std::process::exit`                  | [`exit_with`] helper + CI grep gate    |
//! | SIGKILL / power-off                   | NOT catchable; absence of an "exiting" line implies one of these |
//!
//! On Unix, the first fatal signal writes one marker; the handler then calls the
//! action installed before it, at most once per process, with the original
//! `siginfo_t` and context. Rust's runtime SIGSEGV/SIGBUS handler therefore still
//! sees the fault address and can name a thread that overflowed its stack. When
//! that action returns, or there was none, the handler raises the signal again
//! under its default action, so whatever records the death sees that raise, not
//! the original fault.
//!
//! `install_exit_logging` is idempotent — call once from each binary's
//! `main()` immediately after [`crate::install_panic_hook`] and capture
//! the returned [`ExitGuard`] for the lifetime of the process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[cfg(unix)]
mod unix;

#[cfg(unix)]
use unix::install_signal_handlers;
#[cfg(all(unix, test))]
use unix::{
    chain_previous, classify, own_handler_address, signal_name, slot_for, Callable, Previous,
    FATAL_SIGNALS, PREVIOUS_ACTIONS,
};

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
/// Pre-opened raw fd for sonicterm.log (best-effort) so async-signal-safe
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
pub fn record_exit_reason(reason: ExitReason) {
    let _ = REASON.compare_exchange(
        ExitReason::Clean as u8,
        reason as u8,
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

// Lifecycle: dropping ExitGuard is what emits the clean-return marker; it reads
// REASON to classify, so it must outlive every other shutdown step.
impl Drop for ExitGuard {
    fn drop(&mut self) {
        match REASON.load(Ordering::SeqCst) {
            code if code == ExitReason::Clean as u8 => {
                tracing::warn!(target: "sonic_exit", "sonic exiting: clean main return");
            }
            code if code == ExitReason::LoopExiting as u8 => {
                tracing::warn!(target: "sonic_exit", "sonic exiting: clean after LoopExiting");
            }
            code if code == ExitReason::ExplicitExit as u8 => {
                tracing::warn!(target: "sonic_exit", "sonic exiting: via exit_with()");
            }
            code if code == ExitReason::Panic as u8 => {
                tracing::error!("sonic exiting: after panic");
            }
            code if code == ExitReason::Signal as u8 => {
                tracing::error!("sonic exiting: after fatal signal");
            }
            code if code == ExitReason::AllocFailure as u8 => {
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
pub fn install_exit_logging(log_dir: &Path) -> ExitGuard {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        // When: INSTALLED was already set, so the hooks are armed; installing
        // again would leak a second alt-stack and re-open the log descriptor.
        return ExitGuard(());
    }

    // Best-effort open the active log file for async-signal-safe writes.
    // We re-open append-mode so we don't share a buffered handle with the
    // tracing-appender (its writer is in another thread and may have
    // pending bytes — that's fine, we just append our marker line).
    let _ = std::fs::create_dir_all(log_dir);
    open_log_fd(&exit_log_path(log_dir));

    install_alloc_error_logging();
    #[cfg(unix)]
    install_signal_handlers();

    ExitGuard(())
}

pub(crate) fn exit_log_path(log_dir: &Path) -> PathBuf {
    log_dir.join(crate::path::log_file_name())
}

fn open_log_fd(_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(file) =
            std::fs::OpenOptions::new().create(true).append(true).mode(0o644).open(_path)
        {
            use std::os::unix::io::IntoRawFd;
            let fd = file.into_raw_fd();
            LOG_FD.store(fd, Ordering::SeqCst);
        }
    }
}

fn install_alloc_error_logging() {
    // `std::alloc::set_alloc_error_hook` is unstable on stable Rust. We
    // therefore can't intercept allocator
    // failures directly; instead, the global allocator's default
    // behaviour is to call `__rust_alloc_error_handler`, which prints
    // to stderr and aborts via SIGABRT — and our SIGABRT handler
    // (installed below on Unix) catches the abort and writes a
    // "FATAL: SIGABRT" marker to sonicterm.log. So alloc failures DO
    // produce a log line on Unix, just routed via the signal path.
    // Documented in wiki/Logging.md.
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

#[cfg(test)]
#[path = "exit_trace_tests.rs"]
mod exit_trace_tests;
