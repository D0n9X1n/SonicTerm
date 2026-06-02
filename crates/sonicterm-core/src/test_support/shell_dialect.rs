//! Re-export of `sonicterm_io::test_support::shell_dialect`.
//!
//! Source-of-truth moved to `sonicterm-io` in #469 PR-A. This thin
//! re-export preserves the existing `sonicterm_core::test_support::shell_dialect::*`
//! import path used by examples and the upcoming integration tests.

pub use sonicterm_io::test_support::shell_dialect::*;
