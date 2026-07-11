//! sonicterm-vt — VT/ANSI parser for SonicTerm Terminal.
//!
//! Split out of `sonicterm-core` in the PR-3 refactor.
//! `sonicterm-core` re-exports this crate's contents for back-compat.
//!
//! Depends on `sonicterm-grid` for the `Grid` mutated by the `Performer`.

#![deny(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod vt;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
