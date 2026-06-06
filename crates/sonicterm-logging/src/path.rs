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
/// Resolution: `~/.snoicterm/logs`.
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
    if let Some(home) = home_dir() {
        return home.join(".snoicterm").join("logs");
    }
    PathBuf::from(".snoicterm/logs")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_log_dir_lives_under_dot_snoicterm() {
        let dir = resolve_log_dir();
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some("logs"));
        assert_eq!(
            dir.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()),
            Some(".snoicterm")
        );
    }
}
