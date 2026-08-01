//! Library surface for `sonicterm-mac` — exposes the macOS menubar module
//! for integration tests. The binary entrypoint lives in `main.rs`.

// TODO: add per-item docs and switch to #![deny(missing_docs)].
#![allow(missing_docs)]

#[cfg(test)]
mod bundle_manifest;

#[cfg(target_os = "macos")]
pub mod menubar;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
