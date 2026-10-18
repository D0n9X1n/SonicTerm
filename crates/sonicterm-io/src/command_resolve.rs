//! Program resolution for direct command launches.
//!
//! SonicTerm resolves a launcher's program itself and hands the PTY layer an
//! absolute path, so the spawn never searches a path relative to the child's
//! cwd, never falls back to `$HOME`, and never runs a file through a shell.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Why a direct command launch could not start.
///
/// Every message names the program's file name and the failing rule, never a
/// directory, an argument value or a lower layer's error text.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CommandSpawnError {
    /// The launcher passed an empty program.
    #[error("no program was given")]
    EmptyProgram,
    /// The working directory is missing, relative or not a directory.
    #[error("cannot run {program}: the working directory does not exist or is not a directory")]
    InvalidCwd {
        /// The program's printable file name.
        program: String,
    },
    /// No candidate exists.
    #[error("cannot run {program}: no such program")]
    NotFound {
        /// The program's printable file name.
        program: String,
    },
    /// The target is a directory.
    #[error("cannot run {program}: it is a directory")]
    IsDirectory {
        /// The program's printable file name.
        program: String,
    },
    /// The target exists but is not a regular file.
    #[error("cannot run {program}: it is not a regular file")]
    NotRegularFile {
        /// The program's printable file name.
        program: String,
    },
    /// The target is not executable by this user.
    #[error("cannot run {program}: it is not executable")]
    NotExecutable {
        /// The program's printable file name.
        program: String,
    },
    /// A text file with no `#!` line, which only a shell fallback would run.
    #[error("cannot run {program}: it is neither a native executable nor a script with a #! line")]
    NoInterpreterLine {
        /// The program's printable file name.
        program: String,
    },
    /// A Windows batch file, which `CreateProcessW` would hand to `cmd.exe`.
    #[error("cannot run {program} directly: run batch files with `cmd /c {program}`")]
    BatchScript {
        /// The program's printable file name.
        program: String,
    },
    /// The operating system refused the spawn.
    #[error("cannot run {program}: {reason}")]
    Spawn {
        /// The program's printable file name.
        program: String,
        /// A fixed description of the failure class.
        reason: &'static str,
    },
}

/// The printable file name of `program`, for messages and logs.
pub(crate) fn display_name(program: &OsStr) -> String {
    sonicterm_types::printable_file_name(program)
}

/// Check that `cwd` is an absolute, existing directory.
pub fn validate_cwd(program: &OsStr, cwd: &Path) -> Result<(), CommandSpawnError> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        // When: `cwd` is relative or not an existing directory, refuse rather than substitute a home directory.
        return Err(CommandSpawnError::InvalidCwd { program: display_name(program) });
    }
    Ok(())
}

/// Resolve `program` to an absolute path, searching `path_var` when it names no directory.
///
/// `cwd` must already be valid (see [`validate_cwd`]); a relative program that
/// names a directory resolves against it.
pub fn resolve_program(
    program: &OsStr,
    cwd: &Path,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, CommandSpawnError> {
    if program.is_empty() {
        // When: `program` is empty, there is nothing to resolve or spawn.
        return Err(CommandSpawnError::EmptyProgram);
    }
    validate_cwd(program, cwd)?;
    platform::resolve(program, cwd, path_var)
}

/// Absolute entries of `path_var`; empty and relative entries are skipped.
fn absolute_path_entries(path_var: Option<&OsStr>) -> Vec<PathBuf> {
    path_var
        .map(|value| std::env::split_paths(value).filter(|entry| entry.is_absolute()).collect())
        .unwrap_or_default()
}

/// Unix resolution: execvp's rules without its shell fallback.
#[cfg(unix)]
mod platform {
    use super::*;
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;

    /// Resolve per the Unix rules: a program containing `/` is used as given, else `PATH` is searched.
    pub(super) fn resolve(
        program: &OsStr,
        cwd: &Path,
        path_var: Option<&OsStr>,
    ) -> Result<PathBuf, CommandSpawnError> {
        let name = display_name(program);
        if program.as_bytes().contains(&b'/') {
            // When: `program` contains a slash, it names a file; a relative one resolves against `cwd`.
            let candidate = cwd.join(program);
            return check_candidate(&candidate, &name).map(|()| candidate);
        }
        let mut first_failure = None;
        for entry in absolute_path_entries(path_var) {
            let candidate = entry.join(program);
            match check_candidate(&candidate, &name) {
                Ok(()) => {
                    // When: `check_candidate` returns Ok, this entry holds the first runnable match.
                    return Ok(candidate);
                }
                Err(CommandSpawnError::NotFound { .. }) => {
                    // When: `NotFound` for this `PATH` entry, keep searching later entries.
                }
                Err(error) => {
                    // A candidate that exists but fails a rule is reported only if nothing later runs.
                    first_failure.get_or_insert(error);
                }
            }
        }
        Err(first_failure.unwrap_or(CommandSpawnError::NotFound { program: name }))
    }

    /// Check one candidate: a regular, executable file that is native or starts with `#!`.
    fn check_candidate(candidate: &Path, name: &str) -> Result<(), CommandSpawnError> {
        let Ok(metadata) = std::fs::metadata(candidate) else {
            // When: `metadata` fails, the candidate (or a link's target) does not exist for this user.
            return Err(CommandSpawnError::NotFound { program: name.to_string() });
        };
        if metadata.is_dir() {
            // When: `metadata` describes a directory, exec would fail and the user named a folder.
            return Err(CommandSpawnError::IsDirectory { program: name.to_string() });
        }
        if !metadata.is_file() {
            // When: `metadata` describes a device, socket or FIFO, refuse to execute it.
            return Err(CommandSpawnError::NotRegularFile { program: name.to_string() });
        }
        if !is_executable(candidate) {
            // When: `access(X_OK)` refuses the candidate, exec would fail with a permission error.
            return Err(CommandSpawnError::NotExecutable { program: name.to_string() });
        }
        check_header(candidate, name)
    }

    /// Whether `access(X_OK)` admits `candidate` for this process's real user.
    fn is_executable(candidate: &Path) -> bool {
        let Ok(c_path) = std::ffi::CString::new(candidate.as_os_str().as_bytes()) else {
            // When: `CString` rejects an interior NUL in the path, no exec could name it.
            return false;
        };
        // SAFETY: `c_path` is a NUL-terminated string that outlives the call; `access` only reads it.
        unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
    }

    /// Refuse a file whose header is neither `#!` nor this platform's native executable magic.
    ///
    /// libc's `execvp` retries a file the kernel rejects with `ENOEXEC` through
    /// `/bin/sh`; refusing such a file here keeps that shell path from running.
    /// A file this user may execute but not read is allowed: a shell fallback
    /// could not read it either.
    fn check_header(candidate: &Path, name: &str) -> Result<(), CommandSpawnError> {
        let mut header = Vec::with_capacity(4);
        match std::fs::File::open(candidate) {
            Ok(file) => {
                // When: `file` opened, read its first bytes; a short or failed read leaves fewer, which is refused.
                let _ = file.take(4).read_to_end(&mut header);
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                // When: `PermissionDenied` on an execute-only file means no shell fallback could read it either.
                return Ok(());
            }
            Err(_) => {
                // When: the open fails for another reason, the file vanished or is unusable.
                return Err(CommandSpawnError::NotFound { program: name.to_string() });
            }
        }
        if is_known_header(&header) {
            // When: the header is `#!` or native magic, the kernel executes it without a shell.
            return Ok(());
        }
        Err(CommandSpawnError::NoInterpreterLine { program: name.to_string() })
    }

    /// Whether `header` starts a `#!` script or this platform's native executable.
    pub(super) fn is_known_header(header: &[u8]) -> bool {
        header.starts_with(b"#!") || NATIVE_MAGIC.iter().any(|magic| header.starts_with(magic))
    }

    /// Mach-O thin (32/64-bit, both byte orders) and universal headers.
    #[cfg(target_os = "macos")]
    const NATIVE_MAGIC: &[&[u8]] = &[
        &[0xfe, 0xed, 0xfa, 0xce],
        &[0xce, 0xfa, 0xed, 0xfe],
        &[0xfe, 0xed, 0xfa, 0xcf],
        &[0xcf, 0xfa, 0xed, 0xfe],
        &[0xca, 0xfe, 0xba, 0xbe],
        &[0xbe, 0xba, 0xfe, 0xca],
    ];

    /// The ELF header.
    #[cfg(not(target_os = "macos"))]
    const NATIVE_MAGIC: &[&[u8]] = &[&[0x7f, b'E', b'L', b'F']];
}

/// Windows resolution: an explicit path or a `PATH` search for `.exe`/`.com`, never a batch file.
#[cfg(windows)]
mod platform {
    use super::*;
    use std::path::Component;

    /// Resolve per the Windows rules; batch files are refused before any search result is returned.
    pub(super) fn resolve(
        program: &OsStr,
        cwd: &Path,
        path_var: Option<&OsStr>,
    ) -> Result<PathBuf, CommandSpawnError> {
        let name = display_name(program);
        let program_path = Path::new(program);
        if names_location(program_path) {
            // When: `names_location` sees a separator or drive, use it as given; relative resolves against `cwd`.
            let has_prefix = program_path
                .components()
                .any(|component| matches!(component, Component::Prefix(_)));
            if !program_path.is_absolute() && (program_path.has_root() || has_prefix) {
                // When: `has_root` or `has_prefix` without `is_absolute` means `C:tool` or `\tool`: per-drive state.
                return Err(CommandSpawnError::NotFound { program: name });
            }
            let candidate = cwd.join(program_path);
            return check_candidate(&candidate, &name).map(|()| candidate);
        }
        if is_batch_name(program) {
            // When: `is_batch_name` matches the bare name, refuse before any search can find it.
            return Err(CommandSpawnError::BatchScript { program: name });
        }
        let mut first_failure = None;
        for entry in absolute_path_entries(path_var) {
            for candidate in candidates_in(&entry, program) {
                match check_candidate(&candidate, &name) {
                    Ok(()) => {
                        // When: `check_candidate` returns Ok, this spelling is the first runnable match.
                        return Ok(candidate);
                    }
                    Err(CommandSpawnError::NotFound { .. }) => {
                        // When: `NotFound` for this spelling, try the next spelling or entry.
                    }
                    Err(error) => {
                        // A candidate that exists but fails a rule is reported only if nothing later runs.
                        first_failure.get_or_insert(error);
                    }
                }
            }
        }
        Err(first_failure.unwrap_or(CommandSpawnError::NotFound { program: name }))
    }

    /// Whether `program` contains a separator or a drive prefix.
    fn names_location(program: &Path) -> bool {
        program.components().count() > 1
            || program
                .components()
                .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    }

    /// The spellings tried in one `PATH` entry: the exact name, then `.exe` and `.com`.
    fn candidates_in(entry: &Path, program: &OsStr) -> Vec<PathBuf> {
        let mut candidates = vec![entry.join(program)];
        for extension in [".exe", ".com"] {
            let mut spelled = OsString::from(program);
            spelled.push(extension);
            candidates.push(entry.join(spelled));
        }
        candidates
    }

    /// Check one candidate: an existing regular file that is not a batch file.
    fn check_candidate(candidate: &Path, name: &str) -> Result<(), CommandSpawnError> {
        let Ok(metadata) = std::fs::metadata(candidate) else {
            // When: `metadata` fails, the candidate does not exist.
            return Err(CommandSpawnError::NotFound { program: name.to_string() });
        };
        if metadata.is_dir() {
            // When: `metadata` describes a directory, there is nothing to execute.
            return Err(CommandSpawnError::IsDirectory { program: name.to_string() });
        }
        if !metadata.is_file() {
            // When: `metadata` is neither file nor directory, refuse the device or pipe.
            return Err(CommandSpawnError::NotRegularFile { program: name.to_string() });
        }
        if is_batch_name(candidate.as_os_str()) {
            // When: `is_batch_name` matches the resolved file, `CreateProcessW` would parse it with `cmd.exe`.
            return Err(CommandSpawnError::BatchScript { program: name.to_string() });
        }
        Ok(())
    }
}

/// Whether `program`'s file name ends in `.bat` or `.cmd`, as Windows would read it.
///
/// Windows drops trailing dots and spaces and ignores a `:stream` suffix, so
/// `x.bat.`, `x.bat ` and `x.bat::$DATA` all name the batch file `x.bat`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_batch_name(program: &OsStr) -> bool {
    let lossy = program.to_string_lossy();
    let file_name = lossy.rsplit(['\\', '/']).next().unwrap_or(&lossy);
    let without_stream = file_name.split(':').next().unwrap_or(file_name);
    let trimmed = without_stream.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    trimmed.ends_with(".bat") || trimmed.ends_with(".cmd")
}

/// The `PATH` this process would search, for callers that do not inject one.
pub fn process_path_var() -> Option<OsString> {
    std::env::var_os("PATH")
}

#[cfg(test)]
#[path = "command_resolve_tests.rs"]
mod command_resolve_tests;
