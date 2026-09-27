//! Command runner shared by the macOS and Linux openers: a fixed program and argument list that
//! tests inspect without spawning, run with null stdio and an error on a failed exit status.

use super::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::{Command, Stdio};

/// Platform-neutral command description used by opener tests without spawning handlers.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandSpec {
    pub(super) program: PathBuf,
    pub(super) args: Vec<String>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn run_command(spec: CommandSpec) -> io::Result<()> {
    let status = Command::new(spec.program)
        .args(spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        // When: the fixed native opener returns a failed `status`, surface it instead of reporting a successful click.
        Err(io::Error::other(format!("path opener exited with {status}")))
    }
}
