//! Crash-dump capture.
//!
//! A custom [`tracing_subscriber::Layer`] keeps a fixed-size ring of
//! the last 50 events; on panic, the installed hook serialises the
//! ring + the panic message + a backtrace into
//! `crashes/crash-<utc-iso8601>.log`.
//!
//! After writing the dump, the previously-installed (default) panic
//! hook is invoked so we don't suppress normal abort behaviour.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::Mutex;
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::filter::{FilterExt, LevelFilter};
use tracing_subscriber::layer::{Context, Filter};
use tracing_subscriber::{EnvFilter, Layer};

/// Maximum number of admitted events retained for a crash dump.
pub const RING_CAPACITY: usize = 50;
const RECORD_BYTES: usize = 4096;
const TARGET_BYTES: usize = 256;
const RING_BYTES: usize = 64 * 1024;
const PANIC_BYTES: usize = 4096;
const TRUNCATED: &str = "[truncated]";

/// Intersect user-selected admission with the crash recorder's level and content policy.
pub(crate) fn persistence_filter<S>(selected: EnvFilter) -> impl Filter<S> {
    selected.and(LevelFilter::DEBUG).and(PersistencePolicy)
}

fn persistent_target(target: &str) -> bool {
    target != "sonicterm_font::payload" && !target.starts_with("sonicterm_font::payload::")
}

struct PersistencePolicy;

impl<S> Filter<S> for PersistencePolicy {
    fn enabled(&self, metadata: &Metadata<'_>, _: &Context<'_, S>) -> bool {
        metadata.is_span() || persistent_target(metadata.target())
    }

    fn event_enabled(&self, event: &Event<'_>, _: &Context<'_, S>) -> bool {
        let normalized = event.normalized_metadata();
        persistent_target(normalized.as_ref().unwrap_or_else(|| event.metadata()).target())
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }
}

struct BoundedText<const N: usize> {
    bytes: [u8; N],
    len: usize,
    limit: usize,
    truncated: bool,
}

impl<const N: usize> BoundedText<N> {
    fn new(limit: usize) -> Self {
        Self { bytes: [0; N], len: 0, limit: limit.min(N), truncated: false }
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("formatter preserves UTF-8")
    }

    fn into_boxed_str(self) -> Box<str> {
        self.as_str().into()
    }

    fn from_args(limit: usize, args: std::fmt::Arguments<'_>) -> Self {
        let mut text = Self::new(limit);
        let _ = std::fmt::write(&mut text, args);
        text
    }
}

impl<const N: usize> std::fmt::Write for BoundedText<N> {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        if self.truncated {
            // When: self.truncated marks an exhausted cap, stop cooperative producer formatting.
            return Err(std::fmt::Error);
        }
        if value.len() <= self.limit - self.len {
            // When: value fits the remaining limit, append without truncation or another allocation.
            self.bytes[self.len..self.len + value.len()].copy_from_slice(value.as_bytes());
            self.len += value.len();
            return Ok(());
        }
        let prefix_limit = self.limit.saturating_sub(TRUNCATED.len());
        if self.len > prefix_limit {
            // Prior fragments filled marker space; trim their suffix at a UTF-8 boundary.
            let mut end = prefix_limit;
            while !self.as_str().is_char_boundary(end) {
                end -= 1;
            }
            self.len = end;
        } else {
            // When: self.len fits prefix_limit, preserve its prefix and copy only bounded UTF-8 from value.
            let mut end = prefix_limit - self.len;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            self.bytes[self.len..self.len + end].copy_from_slice(&value.as_bytes()[..end]);
            self.len += end;
        }
        let marker = &TRUNCATED.as_bytes()[..TRUNCATED.len().min(self.limit - self.len)];
        self.bytes[self.len..self.len + marker.len()].copy_from_slice(marker);
        self.len += marker.len();
        self.truncated = true;
        Err(std::fmt::Error)
    }
}

/// Captured rendering of a single tracing event.
#[derive(Debug, Clone)]
struct Captured {
    ts: chrono::DateTime<chrono::Utc>,
    level: tracing::Level,
    target: Box<str>,
    message: Box<str>,
}

static RING: OnceLock<Mutex<Vec<Captured>>> = OnceLock::new();

fn ring() -> &'static Mutex<Vec<Captured>> {
    RING.get_or_init(|| Mutex::new(Vec::with_capacity(RING_CAPACITY)))
}

/// Construct the layer that records into the ring buffer. Install
/// once at startup; cheap to register, ~O(n) memory in `RING_CAPACITY`.
pub fn ring_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    RingLayer
}

struct RingLayer;

impl<S> Layer<S> for RingLayer
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let target =
            BoundedText::<TARGET_BYTES>::from_args(TARGET_BYTES, format_args!("{}", meta.target()))
                .into_boxed_str();
        let mut visitor = MessageVisitor { message: BoundedText::new(RECORD_BYTES - target.len()) };
        event.record(&mut visitor);
        push_captured(Captured {
            ts: chrono::Utc::now(),
            level: *meta.level(),
            target,
            message: visitor.message.into_boxed_str(),
        });
    }
}

fn push_captured(entry: Captured) {
    let mut history = ring().lock();
    let incoming = entry.target.len() + entry.message.len();
    let mut retained: usize =
        history.iter().map(|event| event.target.len() + event.message.len()).sum();
    while history.len() >= RING_CAPACITY || retained + incoming > RING_BYTES {
        // Release exact-sized payloads before admitting the entry that exceeds a ring bound.
        let oldest = history.remove(0);
        retained -= oldest.target.len() + oldest.message.len();
    }
    history.push(entry);
}

struct MessageVisitor {
    message: BoundedText<RECORD_BYTES>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if self.message.truncated || field.name().starts_with("log.") {
            // When: message.truncated or log. fields apply, skip further payload formatting.
            return;
        }
        if self.message.len != 0 {
            // When: message already contains text, separate fields without allocating another string.
            let _ = self.message.write_char(' ');
        }
        if field.name() != "message" {
            // When: field is structured metadata, retain its name alongside the bounded value.
            let _ = write!(self.message, "{}=", field.name());
        }
        if !self.message.truncated {
            // When: the field prefix did not truncate message, its Debug callback still fits the budget.
            let _ = write!(self.message, "{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if self.message.truncated || field.name().starts_with("log.") {
            // When: the budget is exhausted or a transport field is visited, retain no further payload.
            return;
        }
        if self.message.len != 0 {
            // When: message already contains text, separate fields without allocating another string.
            let _ = self.message.write_char(' ');
        }
        if field.name() != "message" {
            // When: field is structured metadata, retain its name alongside the bounded value.
            let _ = write!(self.message, "{}=", field.name());
        }
        let _ = self.message.write_str(value);
    }
}

#[cfg(test)]
#[path = "crash_tests.rs"]
mod crash_tests;

static PANIC_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The session a dump written from this process belongs to.
///
/// Set once at startup, alongside the session marker. A dump carrying no
/// session cannot be attributed to a particular launch, and attributing it to
/// the wrong one would send a reader to the wrong window of the log — so an
/// untagged dump reports its session as absent rather than guessed.
static SESSION_ID: OnceLock<String> = OnceLock::new();

/// Record which session subsequent dumps belong to.
///
/// Call once at startup, after arming the session marker. Later calls are
/// ignored: the first launch identity is the correct one for the life of the
/// process, and a second call could only overwrite it with a later, wrong one.
pub fn set_session_id(id: &str) {
    if valid_session_id(id) {
        // When: `id` passes the bounded allow-list, store the first process session tag for later dumps.
        let _ = SESSION_ID.set(id.to_string());
    }
}

pub(crate) fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The session id dumps are tagged with, or `<none>` when untagged.
///
/// Written into the dump header verbatim, so the sentinel is spelled out
/// rather than left blank — an empty field reads as a truncated file.
fn session_id() -> &'static str {
    SESSION_ID.get().map_or("<none>", String::as_str)
}

/// Install a panic hook that writes
/// `<log_dir>/crashes/crash-<utc-iso8601>.log` and then chains to the
/// previously-installed (default) panic hook. Calling this more than
/// once replaces the wrapper but keeps the originally captured chain.
///
/// The hook is process-wide and fires for panics on EVERY thread —
/// including PTY-reader, render, winit, and tokio worker threads —
/// not just the main thread. This is the cure for the
/// "silent-exit-no-.ips-no-crashes-entry" class of bug where a
/// background-thread panic propagated to abort with no forensic
/// trace.
///
/// In addition to the file dump, a single-line summary is emitted at
/// `ERROR` level on the `tracing` dispatcher so the rolling
/// `sonicterm.log` carries an index entry even when the crash file write
/// itself fails (e.g. read-only home, ENOSPC). The non-blocking
/// appender may drop the marker if the process dies inside the same
/// tick, but in practice the dump file is the authoritative artifact
/// and the marker is just the breadcrumb.
///
/// Set `SONIC_PANIC_ABORT=1` in the environment to skip the chained
/// previous hook and `std::process::abort()` immediately after the
/// dump is written. The default behaviour (chain to `prev`, which is
/// the libstd default = print to stderr + unwind) matches Rust's
/// usual panic semantics so existing `catch_unwind` call-sites keep
/// working.
pub fn install_panic_hook(log_dir: PathBuf) {
    let _ = PANIC_DIR.set(log_dir);
    let prev = std::panic::take_hook();
    let abort = std::env::var_os("SONICTERM_PANIC_ABORT")
        .or_else(|| std::env::var_os("SONIC_PANIC_ABORT"))
        .is_some_and(|v| v == "1");
    std::panic::set_hook(Box::new(move |info| {
        crate::exit_trace::record_exit_reason(crate::exit_trace::ExitReason::Panic);
        let summary = summarize(info);
        // Rolling-log breadcrumb first; file dump is the heavyweight
        // artifact. Use a dedicated target so operators can filter.
        tracing::error!(target: "sonicterm_logging::panic", "{summary}");
        if let Err(e) = write_dump(info) {
            eprintln!("sonicterm-logging: failed to write crash dump: {e}");
        }
        if abort {
            // Skip chained hook; give the non-blocking appender a
            // best-effort moment to flush before the process exits.
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::process::abort();
        }
        prev(info);
    }));
}

fn panic_payload<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    info.payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

fn summarize(info: &std::panic::PanicHookInfo<'_>) -> Box<str> {
    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("<unnamed>");
    let mut text = BoundedText::<PANIC_BYTES>::new(PANIC_BYTES);
    let _ = write!(text, "panic on thread '{thread_name}' at ");
    if let Some(location) = info.location() {
        // When: info has a source location, format it directly inside the summary cap.
        let _ = write!(text, "{}:{}", location.file(), location.line());
    } else {
        // When: the panic has no source location, record absence rather than an inferred caller.
        let _ = text.write_str("<unknown>");
    }
    let _ = write!(text, ": {}", panic_payload(info));
    text.into_boxed_str()
}

fn write_dump(info: &std::panic::PanicHookInfo<'_>) -> std::io::Result<()> {
    let dir = PANIC_DIR.get().cloned().unwrap_or_else(crate::path::crash_dir);
    let crashes = if dir.file_name().is_some_and(|n| n == "crashes") {
        dir
    } else {
        // When: configured `dir` is the log root rather than `crashes`, append the crash subdirectory once.
        dir.join("crashes")
    };
    std::fs::create_dir_all(&crashes)?;
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S%.3fZ");
    let path = crashes.join(format!("crash-{stamp}.log"));
    let mut f = std::fs::File::create(&path)?;

    let payload =
        BoundedText::<PANIC_BYTES>::from_args(PANIC_BYTES, format_args!("{}", panic_payload(info)));

    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("<unnamed>");
    writeln!(f, "== sonic crash dump ==")?;
    writeln!(f, "timestamp: {}", chrono::Utc::now().to_rfc3339())?;
    writeln!(f, "version:   {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(f, "session:   {}", session_id())?;
    writeln!(f, "classification: panic")?;
    writeln!(f, "thread:    {thread_name} ({:?})", thread.id())?;
    if let Some(location) = info.location() {
        writeln!(f, "location:  {}:{}:{}", location.file(), location.line(), location.column())?;
    } else {
        // When: no panic source location exists, preserve the explicit absence marker.
        writeln!(f, "location:  <unknown>")?;
    }
    writeln!(f, "message:   {}", payload.as_str())?;
    writeln!(f)?;
    writeln!(f, "== backtrace ==")?;
    writeln!(f, "{}", std::backtrace::Backtrace::force_capture())?;
    writeln!(f)?;
    writeln!(f, "== last {} tracing events ==", RING_CAPACITY)?;
    let lock = ring().lock();
    for c in lock.iter() {
        writeln!(f, "{} {:>5} {} {}", c.ts.to_rfc3339(), c.level, c.target, c.message)?;
    }
    f.flush()?;
    Ok(())
}

#[doc(hidden)]
/// Test bridge: push a synthetic captured event into the ring without
/// going through the tracing dispatcher. Used by integration tests so
/// they can deterministically assert ring contents.
pub fn __test_push(level: tracing::Level, target: &str, message: &str) {
    let target = BoundedText::<TARGET_BYTES>::from_args(TARGET_BYTES, format_args!("{target}"))
        .into_boxed_str();
    let message = BoundedText::<RECORD_BYTES>::from_args(
        RECORD_BYTES - target.len(),
        format_args!("{message}"),
    )
    .into_boxed_str();
    push_captured(Captured { ts: chrono::Utc::now(), level, target, message });
}

#[doc(hidden)]
/// Test bridge: run the dump-writer with a synthetic panic info-like
/// payload. We can't construct a real [`std::panic::PanicHookInfo`]
/// outside the panic runtime, so this entry point mirrors the dump
/// format for an explicit message + location pair.
pub fn __test_write_dump(dir: &Path, message: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S%.3fZ");
    let path = dir.join(format!("crash-{stamp}.log"));
    let mut f = std::fs::File::create(&path)?;
    writeln!(f, "== sonic crash dump ==")?;
    writeln!(f, "timestamp: {}", chrono::Utc::now().to_rfc3339())?;
    writeln!(f, "version:   {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(f, "session:   {}", session_id())?;
    writeln!(f, "classification: panic")?;
    writeln!(f, "location:  <test>")?;
    let message = BoundedText::<PANIC_BYTES>::from_args(PANIC_BYTES, format_args!("{message}"));
    writeln!(f, "message:   {}", message.as_str())?;
    writeln!(f)?;
    writeln!(f, "== last {} tracing events ==", RING_CAPACITY)?;
    let lock = ring().lock();
    for c in lock.iter() {
        writeln!(f, "{} {:>5} {} {}", c.ts.to_rfc3339(), c.level, c.target, c.message)?;
    }
    f.flush()?;
    Ok(path)
}
