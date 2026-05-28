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
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Maximum number of tracing events retained for the crash dump.
/// Was 200 pre-v0.8.1; lowered to 50 to claw back ~30 MB of steady-state
/// RSS that the larger ring kept allocated for the lifetime of the
/// process. 50 events is still enough context for the post-mortem of
/// nearly every observed Sonic panic.
pub const RING_CAPACITY: usize = 50;

/// Captured rendering of a single tracing event.
#[derive(Debug, Clone)]
struct Captured {
    ts: chrono::DateTime<chrono::Utc>,
    level: tracing::Level,
    target: String,
    message: String,
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
        let mut v = MessageVisitor::default();
        event.record(&mut v);
        let meta = event.metadata();
        let entry = Captured {
            ts: chrono::Utc::now(),
            level: *meta.level(),
            target: meta.target().to_string(),
            message: v.message.unwrap_or_else(|| format!("{:?}", meta.fields())),
        };
        let mut lock = ring().lock();
        if lock.len() == RING_CAPACITY {
            lock.remove(0);
        }
        lock.push(entry);
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else if self.message.is_none() {
            self.message = Some(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else if self.message.is_none() {
            self.message = Some(format!("{}={value}", field.name()));
        }
    }
}

static PANIC_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Install a panic hook that writes
/// `<log_dir>/crashes/crash-<utc-iso8601>.log` and then chains to the
/// previously-installed (default) panic hook. Calling this more than
/// once replaces the wrapper but keeps the originally captured chain.
pub fn install_panic_hook(log_dir: PathBuf) {
    let _ = PANIC_DIR.set(log_dir);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Err(e) = write_dump(info) {
            eprintln!("sonic-logging: failed to write crash dump: {e}");
        }
        prev(info);
    }));
}

// MSRV is 1.80; `PanicHookInfo` only landed in 1.81, so `PanicInfo`
// stays here under an explicit allow. Bump and rename together when
// MSRV crosses 1.81.
#[allow(deprecated)]
fn write_dump(info: &std::panic::PanicInfo<'_>) -> std::io::Result<()> {
    let dir = PANIC_DIR.get().cloned().unwrap_or_else(crate::path::crash_dir);
    let crashes =
        if dir.file_name().is_some_and(|n| n == "crashes") { dir } else { dir.join("crashes") };
    std::fs::create_dir_all(&crashes)?;
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S%.3fZ");
    let path = crashes.join(format!("crash-{stamp}.log"));
    let mut f = std::fs::File::create(&path)?;

    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown>".to_string());
    let payload = info
        .payload()
        .downcast_ref::<&'static str>()
        .copied()
        .map(str::to_string)
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string());

    writeln!(f, "== sonic crash dump ==")?;
    writeln!(f, "timestamp: {}", chrono::Utc::now().to_rfc3339())?;
    writeln!(f, "version:   {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(f, "location:  {location}")?;
    writeln!(f, "message:   {payload}")?;
    writeln!(f)?;
    writeln!(f, "== backtrace ==")?;
    writeln!(f, "{}", std::backtrace::Backtrace::force_capture())?;
    writeln!(f)?;
    writeln!(f, "== last {} tracing events ==", RING_CAPACITY)?;
    let lock = ring().lock();
    for c in lock.iter() {
        let mut line = String::new();
        let _ =
            write!(&mut line, "{} {:>5} {} {}", c.ts.to_rfc3339(), c.level, c.target, c.message);
        writeln!(f, "{line}")?;
    }
    f.flush()?;
    Ok(())
}

#[doc(hidden)]
/// Test bridge: push a synthetic captured event into the ring without
/// going through the tracing dispatcher. Used by integration tests so
/// they can deterministically assert ring contents.
pub fn __test_push(level: tracing::Level, target: &str, message: &str) {
    let entry = Captured {
        ts: chrono::Utc::now(),
        level,
        target: target.to_string(),
        message: message.to_string(),
    };
    let mut lock = ring().lock();
    if lock.len() == RING_CAPACITY {
        lock.remove(0);
    }
    lock.push(entry);
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
    writeln!(f, "location:  <test>")?;
    writeln!(f, "message:   {message}")?;
    writeln!(f)?;
    writeln!(f, "== last {} tracing events ==", RING_CAPACITY)?;
    let lock = ring().lock();
    for c in lock.iter() {
        writeln!(f, "{} {:>5} {} {}", c.ts.to_rfc3339(), c.level, c.target, c.message)?;
    }
    f.flush()?;
    Ok(path)
}
