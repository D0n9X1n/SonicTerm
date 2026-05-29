//! sonic-grid — terminal grid model + hyperlink registry.
//!
//! Split out of `sonic-core` in the PR-3 refactor (issue #121).
//! `sonic-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod grid;
pub mod hyperlink;
pub mod line;
