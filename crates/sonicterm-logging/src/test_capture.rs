//! Shared call-site interest support for scoped tracing captures in tests.
//!
//! The process default records nothing; each capture still supplies its own subscriber, filter, and sink.

use std::sync::OnceLock;
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};

/// Keep every call site eligible for a scoped subscriber without admitting events outside a capture.
struct CaptureInterest;

impl Subscriber for CaptureInterest {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        // First reaches on uncaptured threads must consult their current dispatcher rather than cache `never`.
        Interest::sometimes()
    }

    fn enabled(&self, _: &Metadata<'_>) -> bool {
        false
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        // A low global hint would skip registration before a scoped capture can decide whether it wants an event.
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    // Nothing is forwarded; scoped subscribers retain their own events, span IDs, and parent relationships.
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// Install a silent global dispatcher once so uncaptured first reaches cannot disable scoped captures.
///
/// Installing its dispatch rebuilds cached interest, including call sites registered before this helper ran.
/// This is test support only: do not call it in a process that initializes production logging.
///
/// # Panics
/// Panics if another global subscriber is already installed instead of silently leaving captures disabled.
pub fn ensure_callsite_interest() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        tracing::subscriber::set_global_default(CaptureInterest)
            .expect("test_capture cannot install callsite interest: another global subscriber is already installed");
    });
}

/// Run a capture with the caller's subscriber while keeping process-wide call-site interest capture-safe.
pub fn with_default<T>(
    subscriber: impl Subscriber + Send + Sync + 'static,
    body: impl FnOnce() -> T,
) -> T {
    ensure_callsite_interest();
    tracing::subscriber::with_default(subscriber, body)
}

#[cfg(test)]
#[path = "test_capture_tests.rs"]
mod test_capture_tests;
