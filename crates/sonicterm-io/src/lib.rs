//! sonicterm-io — PTY + process probes for SonicTerm Terminal.
//!
//! `sonicterm-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(clippy::min_ident_chars)]

pub mod command_resolve;
#[cfg(windows)]
mod conpty_runtime;
#[cfg(windows)]
pub mod foreground_proc;
pub mod proc_info;
// The PTY backend and its tests keep their current identifier names.
#[allow(clippy::min_ident_chars)]
pub mod pty;
pub mod pty_backend_feasibility;
mod reply_spill_pool;
mod reply_spool;

pub use reply_spill_pool::{SpillPool, PROCESS_SPILL_BUDGET_BYTES};
pub use reply_spool::{
    is_budget_refusal, PtyReplySender, ReplyBudgetExceeded, ReplySpoolStats, SPILL_BUDGET_BYTES,
};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
