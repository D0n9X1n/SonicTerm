//! Library surface for `sonicterm-mac` — exposes the macOS menubar module
//! for integration tests. The binary entrypoint lives in `main.rs`.

// TODO: add per-item docs and switch to #![deny(missing_docs)] in a follow-up PR.
#![allow(missing_docs)]

#[cfg(target_os = "macos")]
pub mod menubar;
