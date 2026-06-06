//! User-tunable knobs for the logging subsystem.
//!
//! All fields are exposed in `sonicterm.toml` under `[logging]`.

use serde::{Deserialize, Serialize};

/// Retention + level configuration. See field docs for defaults.
///
/// Total disk usage is bounded by
/// `max_file_size_mb * (max_rotated_files + 1)` for log files plus
/// `~max_crash_dumps * <avg crash dump size>` for crash dumps. The
/// shipped defaults yield ≈ 40 MB of logs + ≈ 10 small crash dumps.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// Soft cap (megabytes) for the active `sonicterm.log` before the
    /// cleanup pass treats older daily-rotated files as evictable.
    /// Default: 10.
    pub max_file_size_mb: u64,
    /// Maximum number of *rotated* (non-active) log files to keep on
    /// disk. The active `sonicterm.log` is never counted or deleted by
    /// cleanup. Default: 3.
    pub max_rotated_files: usize,
    /// Delete rotated log files whose mtime is older than this many
    /// days. Set to `0` to disable age-based eviction. Default: 2.
    pub max_age_days: u32,
    /// Maximum number of crash dumps retained under `crashes/`.
    /// Default: 10.
    pub max_crash_dumps: usize,
    /// Delete crash dumps older than this many days. Set to `0` to
    /// disable age-based eviction. Default: 2.
    pub max_crash_age_days: u32,
    /// Optional explicit filter directives string in
    /// [`tracing_subscriber::EnvFilter`] syntax (e.g. `"sonic=debug"`).
    /// When `None`, falls back to `RUST_LOG`, then to the built-in
    /// `DEFAULT_FILTER`. Default: `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            max_file_size_mb: 10,
            max_rotated_files: 3,
            max_age_days: 2,
            max_crash_dumps: 10,
            max_crash_age_days: 2,
            level: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_retention_cleans_after_two_days() {
        let cfg = LoggingConfig::default();
        assert_eq!(cfg.max_age_days, 2);
        assert_eq!(cfg.max_crash_age_days, 2);
    }
}
