//! A process-wide origin for synchronized-output (DEC 2026) deadlines.
//!
//! An `Instant` is not atomic, so a VT worker cannot hand its hold deadline to the event loop
//! without a lock. Both threads measure from one shared origin instead, and the deadline crosses
//! as microseconds since that origin, packed with its epoch tag in an `AtomicU64`.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The shared origin; set on first use, never moved.
pub(in crate::app) static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// The origin every synchronized-output deadline is measured from.
pub(in crate::app) fn origin() -> Instant {
    *ORIGIN.get_or_init(Instant::now)
}

/// Whole microseconds from the origin to `at`; an instant before the origin reads 0.
pub(in crate::app) fn micros_at(at: Instant) -> u64 {
    u64::try_from(at.saturating_duration_since(origin()).as_micros()).unwrap_or(u64::MAX)
}

/// The instant `micros` microseconds after the origin.
pub(in crate::app) fn instant_at_micros(micros: u64) -> Instant {
    origin() + Duration::from_micros(micros)
}

#[cfg(test)]
#[path = "sync_clock_tests.rs"]
mod sync_clock_tests;
