//! sonicterm-grid — terminal grid model + hyperlink registry.
//!
//! Split out of `sonicterm-core` in the PR-3 refactor.
//! `sonicterm-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod grid;
pub mod hyperlink;
pub mod line;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
