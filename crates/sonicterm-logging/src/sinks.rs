//! Sink construction is split into this module purely for testability.
//!
//! The shipped wiring in [`crate::init`] inlines the appender / layer
//! builders because each one needs to feed its `WorkerGuard` into the
//! returned [`crate::LoggingGuard`] and that ownership dance does not
//! survive a clean function extraction. The constants and helpers
//! here document the *intent* so downstream code (and reviewers) have
//! a single named home for them.

/// Suffix appended to the active `sonicterm.log` by `tracing-appender`'s
/// daily rotation. Exposed so [`crate::cleanup`] can recognise rotated
/// files unambiguously.
pub const ROTATED_PREFIX: &str = "sonicterm.log.";
