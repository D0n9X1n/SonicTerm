//! Test-only helpers exposed behind `cfg(feature = "test_support")`.
//!
//! These modules are NOT production API. They exist so the e2e gate
//! examples (`pty_dump`, `pty_dump_unicode`) and the Windows integration
//! test in `sonicterm-io` can share the per-shell command emitter without
//! duplicating code.

pub mod shell_dialect;
