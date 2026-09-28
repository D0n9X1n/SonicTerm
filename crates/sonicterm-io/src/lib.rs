//! sonicterm-io — PTY + process probes for SonicTerm Terminal.
//!
//! `sonicterm-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(clippy::min_ident_chars)]

#[cfg(windows)]
pub mod foreground_proc;
pub mod proc_info;
// The PTY backend and its tests keep their current identifier names.
#[allow(clippy::min_ident_chars)]
pub mod pty;
pub mod pty_backend_feasibility;
mod reply_spool;

pub use reply_spool::PtyReplySender;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
