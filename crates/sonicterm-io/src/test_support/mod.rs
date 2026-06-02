//! Test-only helpers exposed behind `cfg(feature = "test_support")`.
//!
//! These modules are NOT production API. They exist so the e2e gate
//! examples (`pty_dump`, `pty_dump_unicode`) and the Windows integration
//! test can share the per-shell command emitter without duplicating code.
//!
//! Source-of-truth lives here in `sonicterm-io`; `sonicterm-core` re-exports
//! for back-compat (see #469 PR-A).

pub mod shell_dialect;
