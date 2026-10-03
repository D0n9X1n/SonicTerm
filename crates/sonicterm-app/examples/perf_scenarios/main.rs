#![warn(clippy::min_ident_chars)]
//! Isolated performance scenarios run against the real `App` in a scratch directory.
//!
//! `--list` prints the scenario catalog as JSON on every platform. `--run`
//! measures one scenario on macOS and Windows and prints `NOT_EXERCISED`
//! elsewhere. On Windows the same binary is also each pane's role program. This
//! root declares no global allocator, like every shipping binary;
//! `alloc_main.rs` runs the same modules under a counting allocator.

mod cli;
#[cfg(any(target_os = "macos", windows))]
mod probe;
#[cfg(any(target_os = "macos", windows, test))]
mod record;
#[cfg(any(target_os = "macos", windows, test))]
mod scan_throttle;
mod scenarios;
#[cfg(any(target_os = "macos", windows, test))]
mod waits;
#[cfg(any(target_os = "macos", windows, test))]
mod workload;

fn main() -> std::process::ExitCode {
    cli::run(None)
}
