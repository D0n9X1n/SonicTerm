//! Resolve the platform log directory, log file name, and crash dump
//! directory. All paths are stable for the lifetime of the process.

use std::path::PathBuf;
use std::sync::OnceLock;

static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// File name of the active (non-rotated) log file. Rotated files
/// receive a `.YYYY-MM-DD` suffix appended by `tracing-appender`.
pub const fn log_file_name() -> &'static str {
    "sonicterm.log"
}

/// Environment variable that replaces the whole log directory.
///
/// Diagnostic runs set it to a scratch directory so their logs, crash dumps and
/// session markers never mix with the user's own, while `HOME` stays unchanged
/// for the shell running inside the terminal.
pub const LOG_DIR_ENV: &str = "SONICTERM_LOG_DIR";

/// Absolute path of the directory holding `sonicterm.log` and `crashes/`.
///
/// Resolution: [`LOG_DIR_ENV`] when set and non-empty, else `~/.sonicterm/logs`.
///
/// On the first call, the result is memoised — subsequent calls are
/// O(1) and return the same path even if env vars change later. This
/// matters because the panic hook reads the log dir from a stable
/// snapshot rather than from a possibly-poisoned env at crash time.
pub fn log_dir() -> PathBuf {
    LOG_DIR.get_or_init(resolve_log_dir).clone()
}

/// Absolute path of the crash-dump subdirectory (`<log_dir>/crashes`).
/// Caller is responsible for `create_dir_all` before writing.
pub fn crash_dir() -> PathBuf {
    log_dir().join("crashes")
}

/// The crash subdirectory of an explicitly-named log directory.
///
/// Distinct from [`crash_dir`], which resolves through the memoised global
/// path. That memoisation is deliberate for the panic hook — it must not read
/// a possibly-poisoned environment at crash time — but it makes the global
/// unusable for any caller that needs to work against a directory chosen at
/// runtime, which includes every test that must not write into a real
/// `~/.sonicterm`.
pub fn crash_dir_in(log_dir: &std::path::Path) -> PathBuf {
    log_dir.join("crashes")
}

fn resolve_log_dir() -> PathBuf {
    resolve_log_dir_with(|name| std::env::var_os(name))
}

/// Resolve the log directory from an injected environment lookup.
///
/// Separate from [`resolve_log_dir`] so tests exercise every branch without
/// mutating the process environment, which other tests read concurrently.
fn resolve_log_dir_with(lookup: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    if let Some(dir) = lookup(LOG_DIR_ENV).filter(|value| !value.is_empty()) {
        // When: `LOG_DIR_ENV` names a directory, it replaces the whole default tree.
        return PathBuf::from(dir);
    }
    let home = lookup("HOME").or_else(|| lookup("USERPROFILE"));
    if let Some(home) = home {
        // When: `home` resolves from the environment, keep logs under that user's SonicTerm directory.
        return PathBuf::from(home).join(".sonicterm").join("logs");
    }
    PathBuf::from(".sonicterm/logs")
}

/// Atomically replace `destination` with the complete file at `source`.
///
/// Both paths must be on the same filesystem. Unix `rename` replaces an
/// existing destination atomically. Windows requires `MoveFileExW` with the
/// replace flag; deleting first would leave a window where a hard kill erases
/// the last usable diagnostic snapshot.
pub(crate) fn replace_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        std::fs::rename(source, destination)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: both UTF-16 paths are NUL-terminated and live through the call; flags request same-volume replacement.
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(std::io::Error::other)
    }
}

#[cfg(test)]
#[path = "path_tests.rs"]
mod path_tests;
