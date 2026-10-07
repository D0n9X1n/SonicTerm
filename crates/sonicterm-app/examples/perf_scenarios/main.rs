#![warn(clippy::min_ident_chars)]
//! Isolated performance scenarios run against the real `App` in a scratch directory.
//!
//! `--list` prints the scenario catalog as JSON on every platform. `--run`
//! measures one scenario on macOS and Windows and prints `NOT_EXERCISED`
//! elsewhere. On Windows the same binary is also each pane's role program, and
//! `--capture-delivery` replays one scenario's delivery through ConPTY. This
//! root declares no global allocator, like every shipping binary;
//! `alloc_main.rs` runs the same modules under a counting allocator.

#[cfg(any(target_os = "macos", windows, test))]
mod atlas_retry;
#[cfg(any(target_os = "macos", windows, test))]
mod attribution;
mod cli;
#[cfg(any(target_os = "macos", windows, test))]
mod counters;
#[cfg(any(windows, test))]
mod delivery;
mod digest;
#[cfg(any(target_os = "macos", windows, test))]
mod dispatch_timeline;
#[cfg(any(target_os = "macos", windows, test))]
mod guard_transport;
#[cfg(any(target_os = "macos", windows))]
mod probe;
#[cfg(any(target_os = "macos", windows, test))]
mod record;
#[cfg(any(target_os = "macos", windows, test))]
mod scan_throttle;
mod scenarios;
#[cfg(test)]
mod test_allocator;
#[cfg(any(target_os = "macos", windows, test))]
mod timeline;
#[cfg(any(target_os = "macos", windows, test))]
mod transition;
#[cfg(any(target_os = "macos", windows, test))]
mod waits;
#[cfg(any(target_os = "macos", windows, test))]
mod workload;

fn main() -> std::process::ExitCode {
    cli::run(None)
}
