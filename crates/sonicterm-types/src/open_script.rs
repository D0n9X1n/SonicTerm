//! Typed paths for script-file open requests.

use std::path::{Path, PathBuf};

/// A script path as supplied by the OS and as resolved for launching.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenScriptRequest {
    /// Original spelling retained for diagnostics and user-facing messages.
    pub original_path: PathBuf,
    /// Absolute lexical path used for pane cwd and command-draft formatting.
    pub launch_path: PathBuf,
}

/// Why an open-script path could not be resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenScriptResolveError {
    /// A relative path was supplied but the process's initial cwd was unavailable.
    InitialCwdUnavailable,
    /// The supplied initial cwd was not absolute.
    InitialCwdNotAbsolute,
}

impl OpenScriptRequest {
    /// Resolve a path lexically against an explicit initial process cwd.
    pub fn resolve(
        original_path: PathBuf,
        initial_cwd: &Path,
    ) -> Result<Self, OpenScriptResolveError> {
        let launch_path = if original_path.is_absolute() {
            original_path.clone()
        } else {
            // When: `original_path` is relative, resolve it against the process cwd captured at launch.
            if !initial_cwd.is_absolute() {
                // When: `initial_cwd` is relative, joining it cannot produce the absolute launch contract.
                return Err(OpenScriptResolveError::InitialCwdNotAbsolute);
            }
            initial_cwd.join(&original_path)
        };
        Ok(Self { original_path, launch_path })
    }

    /// Resolve using a lazily queried initial cwd for relative paths only.
    pub fn resolve_with_cwd_lookup<F>(
        original_path: PathBuf,
        cwd_lookup: F,
    ) -> Result<Self, OpenScriptResolveError>
    where
        F: FnOnce() -> Option<PathBuf>,
    {
        if original_path.is_absolute() {
            // When: `original_path` is already absolute, avoid querying cwd so launch cannot fail on unrelated process state.
            return Ok(Self { launch_path: original_path.clone(), original_path });
        }
        let initial_cwd = cwd_lookup().ok_or(OpenScriptResolveError::InitialCwdUnavailable)?;
        Self::resolve(original_path, &initial_cwd)
    }

    /// Parent directory used as the script pane's cwd.
    #[must_use]
    pub fn pane_cwd(&self) -> Option<&Path> {
        self.launch_path.parent()
    }
}

#[cfg(test)]
#[path = "open_script_tests.rs"]
mod open_script_tests;
