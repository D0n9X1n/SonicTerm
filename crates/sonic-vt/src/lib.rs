//! sonic-vt — VT/ANSI parser for Sonic Terminal.
//!
//! Split out of `sonic-core` in the PR-3 refactor (issue #121).
//! `sonic-core` re-exports this crate's contents for back-compat.
//!
//! Depends on `sonic-grid` for the `Grid` mutated by the `Performer`.

#![deny(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod vt;
