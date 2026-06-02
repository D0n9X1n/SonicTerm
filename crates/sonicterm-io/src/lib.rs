//! sonicterm-io — PTY + process probes for SonicTerm Terminal.
//!
//! Split out of `sonicterm-core` in the PR-3 refactor (issue #121).
//! `sonicterm-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
pub mod foreground_proc;
pub mod proc_info;
pub mod pty;
pub mod ssh;

#[cfg(feature = "test_support")]
pub mod test_support;
