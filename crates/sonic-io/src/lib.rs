//! sonic-io — PTY + process probes for Sonic Terminal.
//!
//! Split out of `sonic-core` in the PR-3 refactor (issue #121).
//! `sonic-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
pub mod foreground_proc;
pub mod proc_info;
pub mod pty;
pub mod ssh;
