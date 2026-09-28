//! Code shared by the macOS and Linux openers: target classification that follows symlinks, and a
//! command runner with a fixed program and argument list that tests inspect without spawning, run
//! with null stdio and an error on a failed exit status.

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

/// Classify a local path after resolving every symlink in it: `Missing` when nothing exists at the
/// path, and `Blocked` for a dangling or looping link and for anything but a file or folder.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn classify_followed_target(path: &Path) -> Result<PathKind, PathOpenDecision> {
    std::fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            PathOpenDecision::Missing
        } else {
            // When: `error.kind()` is not `NotFound`, deny an unreadable entry instead of inferring its identity.
            PathOpenDecision::Blocked
        }
    })?;
    // The entry exists, so a link that does not resolve (dangling or looping) blocks rather than reading as missing.
    let metadata = std::fs::metadata(path).map_err(|_| PathOpenDecision::Blocked)?;
    if metadata.is_file() {
        Ok(PathKind::File)
    } else if metadata.is_dir() {
        // When: `metadata.is_dir()` identifies a folder, preserve that kind for activation-time revalidation.
        Ok(PathKind::Directory)
    } else {
        // When: neither `metadata.is_file()` nor `metadata.is_dir()` holds, block sockets, devices, and other special entries.
        Err(PathOpenDecision::Blocked)
    }
}
