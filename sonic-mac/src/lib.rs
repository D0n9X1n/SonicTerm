//! Library surface for `sonic-mac` — exposes the macOS menubar module
//! for integration tests. The binary entrypoint lives in `main.rs`.

#[cfg(target_os = "macos")]
pub mod menubar;
