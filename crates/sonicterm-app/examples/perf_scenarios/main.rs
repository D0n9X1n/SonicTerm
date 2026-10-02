#![warn(clippy::min_ident_chars)]
//! Isolated performance scenarios run against the real `App` in a scratch directory.
//!
//! `--list` prints the scenario catalog as JSON on every platform. `--run`
//! measures one scenario on macOS and prints `NOT_EXERCISED` elsewhere. This
//! root declares no global allocator, like every shipping binary;
//! `alloc_main.rs` runs the same modules under a counting allocator.

mod cli;
#[cfg(target_os = "macos")]
mod probe;
#[cfg(any(target_os = "macos", test))]
mod record;
#[cfg(any(target_os = "macos", test))]
mod scan_throttle;
mod scenarios;
#[cfg(any(target_os = "macos", test))]
mod waits;
#[cfg(any(target_os = "macos", test))]
mod workload;

fn main() -> std::process::ExitCode {
    cli::run(None)
}
