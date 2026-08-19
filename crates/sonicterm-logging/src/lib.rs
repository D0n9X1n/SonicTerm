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
//! - `~/.sonicterm/logs/sonicterm.log`
//!
//! ## Retention
//!
//! See [`LoggingConfig`] for the knobs that bound disk usage. Defaults clean
//! logs and crash dumps older than 2 days.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod breadcrumbs;
pub mod cleanup;
pub mod config;
pub mod crash;
pub mod exit_trace;
pub mod path;
pub mod postmortem;
pub mod process_memory;
pub mod session_state;
pub mod sinks;

pub use cleanup::{
    cleanup_artifacts, cleanup_log_files, cleanup_old_files, cleanup_old_files_async,
    clear_all_rotated,
};
pub use config::{LogLevel, LoggingConfig};
pub use crash::install_panic_hook;
pub use exit_trace::{exit_with, install_exit_logging, record_loop_exiting, ExitGuard, ExitReason};
pub use path::{crash_dir, log_dir, log_file_name};
pub use process_memory::{MemoryDelta, MemoryMetric, ProcessMemory};

use std::{io, path::Path};

use tracing_subscriber::{
    layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer, Registry,
};

/// Held by `main()` for the lifetime of the process to keep the
/// background appender thread alive. Drop flushes any pending writes.
pub struct LoggingGuard {
    /// Tracing-appender's `WorkerGuard` — must be alive while logging.
    _file_guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Custom `tracing` targets the app emits that are not crate names.
///
/// `EnvFilter` admits a target only when a directive names it. A target that
/// appears in no directive is silently never enabled — `tracing::enabled!`
/// returns false and the call site compiles to nothing observable, with no
/// warning at build or run time.
///
/// That bit the `memory` target: it had 25 call sites, a documented wiki
/// procedure telling users to set `level = "debug"`, and no directive. Setting
/// the documented level produced no output, and because the retention sampling
/// path is itself gated on `enabled!(target: "memory", …)`, the governor was
/// charged nothing in any shipped session.
///
/// Listing them in one place, and asserting that list against the source, is
/// what stops the next target from being added the same way.
pub const CUSTOM_DEBUG_TARGETS: &[&str] = &[
    "memory",
    "memory::reclaimed",
    "state_machine",
    "state_machine.log",
    "render_timing",
    "tear_out_timing",
    "sonic::glyph_atlas",
    "sonic::render::glyph",
];

/// The target for reclamation that **destroys something the user can see**.
///
/// Separate from `memory` because the two answer different questions. `memory`
/// is diagnostics — 34 call sites describing what a session holds — and is
/// rightly off unless someone is investigating. This target carries the events
/// where SonicTerm discarded a user's data to stay within a budget: an
/// abandoned image transfer, a trimmed pane's images. Those are not
/// diagnostics. A user whose image vanished is owed a reason whether or not
/// they had the foresight to enable debug logging first.
///
/// Admitted at every level, including the default `warn`, for the same reason
/// `sonic::glyph_atlas` is: an exhaustion the user experiences must reach the
/// log the user actually has.
pub const MEMORY_RECLAIMED_TARGET: &str = "memory::reclaimed";

/// Default user-facing `warn` filter. `sonic_exit` stays WARN-on so exit
/// markers survive; noisy renderer/backend crates stay pinned to WARN.
///
/// `memory::reclaimed=warn` is here rather than only at `debug` because it
/// reports data the user has already lost. This constant is also the
/// parse-failure fallback, so a malformed `RUST_LOG` cannot silence it either.
pub const DEFAULT_FILTER: &str = "sonic_exit=warn,sonic=warn,sonicterm=warn,sonicterm_vt=warn,\
     sonicterm_grid=warn,memory::reclaimed=warn,wgpu=warn,naga=warn";

/// The `EnvFilter` directive string a configured log level produces.
///
/// Public so callers can assert against the filter a shipped session actually
/// uses. A test that writes its own directive string proves the gate behind it
/// works and says nothing about whether any configured level opens that gate —
/// which is exactly the defect this function's contents once carried.
#[must_use]
pub fn filter_for_level(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => DEFAULT_FILTER,
        // `memory=info` admits the aggregate snapshot and nothing below it.
        // The per-pane and per-renderer lines emit at DEBUG, so this directive
        // opens exactly one line per sample: the one that answers "what is
        // this session holding" without the detail an investigating session
        // wants. A user who has to know to set `debug` first has already lost
        // the session they wanted to explain.
        LogLevel::Info => {
            "sonic_exit=warn,sonic=warn,sonicterm=info,sonicterm_vt=warn,sonicterm_grid=warn,\
             memory=info,memory::reclaimed=warn,wgpu=warn,naga=warn"
        }
        // Every custom target is admitted here, not just the two that happened
        // to be added when they were introduced. `debug` is the level the
        // documentation tells a user to set when investigating, so it has to
        // turn on the diagnostics that documentation describes.
        LogLevel::Debug => {
            "sonic_exit=warn,sonic=debug,sonicterm=debug,sonicterm_vt=warn,sonicterm_grid=warn,\
             memory=debug,memory::reclaimed=debug,state_machine=debug,state_machine.log=debug,\
             render_timing=debug,tear_out_timing=debug,\
             wgpu=warn,naga=warn"
        }
    }
}

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
    init_in(cfg, &path::log_dir())
}

/// Initialize tracing in an explicitly selected log directory.
///
/// Platform diagnostics and isolated runtime probes use this when their session
/// state must not share the default user log directory.
///
/// # Errors
///
/// Returns an [`io::Error`] when `dir` cannot be created.
pub fn init_in(cfg: &LoggingConfig, dir: &Path) -> io::Result<LoggingGuard> {
    std::fs::create_dir_all(dir)?;

    // Size-based rotation isn't a native tracing-appender feature, so
    // we use daily rotation as the appender's own knob and rely on
    // `cleanup_old_files` to enforce size + count + age caps. Rotated
    // file names follow `sonicterm.log.YYYY-MM-DD`.
    let file_appender = tracing_appender::rolling::daily(dir, path::log_file_name());
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter_src =
        std::env::var("RUST_LOG").unwrap_or_else(|_| filter_for_level(cfg.level).to_string());
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

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;

pub mod snapshot_format;

#[cfg(test)]
#[path = "redaction_tests.rs"]
mod redaction_tests;
