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

/// Absolute path of the directory holding `sonicterm.log` and `crashes/`.
///
/// Resolution order:
/// 1. `SONICTERM_LOG_DIR` env var (used by tests and ops overrides);
/// 2. macOS: `~/Library/Logs/SonicTerm`;
/// 3. Windows: `%LOCALAPPDATA%\SonicTerm\Logs`;
/// 4. otherwise: `$XDG_STATE_HOME/sonicterm/logs` (or
///    `~/.local/state/sonicterm/logs`).
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

fn resolve_log_dir() -> PathBuf {
    if let Some(p) =
        std::env::var_os("SONICTERM_LOG_DIR").or_else(|| std::env::var_os("SONIC_LOG_DIR"))
    {
        return PathBuf::from(p);
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = home_dir() {
            return home.join("Library/Logs/SonicTerm");
        }
    } else if cfg!(target_os = "windows") {
        if let Some(la) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(la).join("SonicTerm").join("Logs");
        }
        if let Some(home) = home_dir() {
            return home.join("AppData/Local/SonicTerm/Logs");
        }
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    state.join("sonicterm/logs")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}
