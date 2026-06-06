//! sonicterm-logging — SonicTerm logging subsystem.
//!
//! This crate is infrastructure only: it wires up [`tracing`] with two
//! sinks (stderr at WARN+ and a rolling file at INFO+ by default),
//! enforces a hard disk-usage budget via [`cleanup`], and installs a
//! panic hook that dumps the last ~50 tracing events plus a backtrace
//! into `crashes/crash-<utc-iso8601>.log` for post-mortem debugging.
//!
//! ## Usage
//!
//! ```no_run
//! use sonicterm_logging::{init, install_panic_hook, log_dir, LoggingConfig};
//! let cfg = LoggingConfig::default();
//! let _guard = init(&cfg).expect("init logger");
//! install_panic_hook(log_dir());
//! tracing::info!(version = env!("CARGO_PKG_VERSION"), "sonic started");
//! ```
//!
//! Drop the returned [`LoggingGuard`] only at process exit — the
//! background appender thread flushes on drop.
//!
//! ## Log location
//!
//! - `~/.snoicterm/logs/sonicterm.log`
//!
//! ## Retention
//!
//! See [`LoggingConfig`] for the knobs that bound disk usage. Defaults clean
//! logs and crash dumps older than 2 days.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod cleanup;
pub mod config;
pub mod crash;
pub mod exit_trace;
pub mod path;
pub mod sinks;

pub use cleanup::{cleanup_old_files, cleanup_old_files_async, clear_all_rotated};
pub use config::LoggingConfig;
pub use crash::install_panic_hook;
pub use exit_trace::{exit_with, install_exit_logging, record_loop_exiting, ExitGuard, ExitReason};
pub use path::{crash_dir, log_dir, log_file_name};

use std::io;

use tracing_subscriber::{
    layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer, Registry,
};

/// Held by `main()` for the lifetime of the process to keep the
/// background appender thread alive. Drop flushes any pending writes.
pub struct LoggingGuard {
    /// Tracing-appender's `WorkerGuard` — must be alive while logging.
    _file_guard: tracing_appender::non_blocking::WorkerGuard,
}

/// The default per-target filter applied when neither `RUST_LOG` nor
/// `LoggingConfig::level` is set. Top-level `sonicterm` floor is INFO so
/// diagnostic logs from the renamed `sonicterm_*` crates (font discovery,
/// app loop bootstrap, platform glue) reach the rolling file. The noisy
/// `sonicterm_vt` and `sonicterm_grid` crates are pinned to WARN to keep
/// steady-state chatter out of the file (this is the original v0.8.1
/// RSS-driven decision, preserved here per-crate rather than via a
/// blanket umbrella). The `sonic_exit` target (emitted from this crate's
/// own exit-trace module) is kept explicitly WARN-on so exit markers
/// survive the default filter. Post-#430 rename: the prior `sonic=warn`
/// rule matched no crate after `sonic-*` → `sonicterm-*` and silently
/// dropped every INFO log from the renamed crates — see issue #448.
pub const DEFAULT_FILTER: &str =
    "sonic_exit=warn,sonicterm=info,sonicterm_vt=warn,sonicterm_grid=warn,wgpu=warn,naga=warn";

/// Initialize tracing with a stderr layer (WARN+) and a rolling file
/// layer (INFO+ default; overridden by `RUST_LOG` or `cfg.level`).
///
/// Returns a guard whose lifetime keeps the background appender thread
/// running. Idempotent in the sense that re-initialisation is a no-op
/// (the second call returns its own guard but the global dispatcher
/// keeps the first subscriber). Callers MUST keep the guard alive for
/// the lifetime of the process or pending writes may be lost.
///
/// # Errors
///
/// Returns an [`io::Error`] when the log directory cannot be created
/// (e.g., read-only home, permissions denied). Never panics — even on
/// a hostile filesystem the caller can choose to continue with a
/// no-op log setup.
pub fn init(cfg: &LoggingConfig) -> io::Result<LoggingGuard> {
    let dir = path::log_dir();
    std::fs::create_dir_all(&dir)?;

    // Size-based rotation isn't a native tracing-appender feature, so
    // we use daily rotation as the appender's own knob and rely on
    // `cleanup_old_files` to enforce size + count + age caps. Rotated
    // file names follow `sonicterm.log.YYYY-MM-DD`.
    let file_appender = tracing_appender::rolling::daily(&dir, path::log_file_name());
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter_src = cfg
        .level
        .clone()
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| DEFAULT_FILTER.to_string());
    let file_filter =
        EnvFilter::try_new(&filter_src).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let stderr_filter = EnvFilter::try_new(&filter_src)
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
        // Always upgrade the stderr-side floor to WARN so users
        // running with `RUST_LOG=debug` don't get a screenful on the
        // console — file still gets DEBUG.
        .add_directive("warn".parse().expect("WARN parses"));

    let ring = crash::ring_layer();

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(file_writer)
        .with_filter(file_filter);
    let stderr_layer =
        tracing_subscriber::fmt::layer().with_writer(io::stderr).with_filter(stderr_filter);

    let _ = Registry::default().with(ring).with(file_layer).with(stderr_layer).try_init();

    Ok(LoggingGuard { _file_guard: guard })
}
