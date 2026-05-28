//! Retention enforcement for log files and crash dumps.
//!
//! Cleanup is **fail-soft**: every filesystem error is logged at WARN
//! and swallowed so a hostile log directory cannot crash the app at
//! startup.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::config::LoggingConfig;
use crate::path::{crash_dir, log_file_name};
use crate::sinks::ROTATED_PREFIX;

/// Run a cleanup pass over `log_dir` and its `crashes/` subdirectory.
///
/// - Caps rotated log files at `cfg.max_rotated_files` (oldest mtime
///   evicted first).
/// - Deletes rotated log files older than `cfg.max_age_days`
///   (`0` disables age eviction).
/// - Caps crash dumps at `cfg.max_crash_dumps`.
/// - Deletes crash dumps older than `cfg.max_crash_age_days`
///   (`0` disables age eviction).
///
/// The active `sonic.log` is **never** deleted.
pub fn cleanup_old_files(log_dir: &Path, cfg: &LoggingConfig) {
    enforce_rotated_logs(log_dir, cfg);
    enforce_crash_dumps(log_dir, cfg);
}

/// Spawn `cleanup_old_files` on a background thread. Used by the
/// platform entry points so a slow filesystem cannot stall startup.
pub fn cleanup_old_files_async(log_dir: PathBuf, cfg: &LoggingConfig) {
    let cfg = cfg.clone();
    std::thread::Builder::new()
        .name("sonic-logging-cleanup".to_string())
        .spawn(move || cleanup_old_files(&log_dir, &cfg))
        .map(|_| ())
        .unwrap_or_else(|e| tracing::warn!("failed to spawn cleanup thread: {e}"));
}

/// Aggressive cleanup invoked from the Help → Clear Old Logs menu
/// item: removes **every** rotated log file (i.e., every file whose
/// name starts with `sonic.log.` *except* the most recent one — the
/// active file `tracing-appender` is writing to) and **every** crash
/// dump. Returns a `(files_removed, bytes_removed)` pair for the UI
/// toast.
pub fn clear_all_rotated(log_dir: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let active = active_log(log_dir);
    if let Ok(read) = std::fs::read_dir(log_dir) {
        for entry in read.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else { continue };
            if name_str == log_file_name() {
                continue;
            }
            if !name_str.starts_with(ROTATED_PREFIX) {
                continue;
            }
            let path = entry.path();
            if Some(&path) == active.as_ref() {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    files += 1;
                    bytes += size;
                }
                Err(e) => tracing::warn!("cleanup: remove {path:?} failed: {e}"),
            }
        }
    }
    let crashes = crash_dir_from(log_dir);
    if let Ok(read) = std::fs::read_dir(&crashes) {
        for entry in read.flatten() {
            let path = entry.path();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    files += 1;
                    bytes += size;
                }
                Err(e) => tracing::warn!("cleanup: remove {path:?} failed: {e}"),
            }
        }
    }
    (files, bytes)
}

/// The `tracing-appender::rolling::daily` appender never produces a
/// bare `sonic.log` — every file is named `sonic.log.YYYY-MM-DD`. The
/// active file (the one being written to right now) is, by
/// construction, the one with the most recent mtime. We never delete
/// that file from cleanup paths.
fn active_log(log_dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(log_dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name_str = name.to_str()?;
            if !name_str.starts_with(ROTATED_PREFIX) && name_str != log_file_name() {
                return None;
            }
            let mtime = e.metadata().ok().and_then(|m| m.modified().ok())?;
            Some((e.path(), mtime))
        })
        .collect();
    candidates.sort_by_key(|(_, m)| *m);
    candidates.pop().map(|(p, _)| p)
}

fn enforce_rotated_logs(log_dir: &Path, cfg: &LoggingConfig) {
    let active = active_log(log_dir);
    let mut rotated: Vec<(PathBuf, SystemTime)> = match std::fs::read_dir(log_dir) {
        Ok(read) => read
            .flatten()
            .filter_map(|e| {
                let name = e.file_name();
                let name_str = name.to_str()?;
                if name_str == log_file_name() {
                    return None;
                }
                if !name_str.starts_with(ROTATED_PREFIX) {
                    return None;
                }
                let path = e.path();
                if Some(&path) == active.as_ref() {
                    return None;
                }
                let mtime = e.metadata().ok().and_then(|m| m.modified().ok())?;
                Some((path, mtime))
            })
            .collect(),
        Err(e) => {
            tracing::warn!("cleanup: read {log_dir:?} failed: {e}");
            return;
        }
    };
    // Oldest first.
    rotated.sort_by_key(|(_, m)| *m);

    let now = SystemTime::now();
    if cfg.max_age_days > 0 {
        let cutoff = Duration::from_secs(u64::from(cfg.max_age_days) * 86_400);
        rotated.retain(|(p, mtime)| {
            let age = now.duration_since(*mtime).unwrap_or_default();
            if age > cutoff {
                if let Err(e) = std::fs::remove_file(p) {
                    tracing::warn!("cleanup: remove {p:?} failed: {e}");
                }
                false
            } else {
                true
            }
        });
    }

    while rotated.len() > cfg.max_rotated_files {
        // Pop the oldest (front of sorted vec).
        let (path, _) = rotated.remove(0);
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("cleanup: remove {path:?} failed: {e}");
        }
    }
}

fn enforce_crash_dumps(log_dir: &Path, cfg: &LoggingConfig) {
    let crashes = crash_dir_from(log_dir);
    let mut dumps: Vec<(PathBuf, SystemTime)> = match std::fs::read_dir(&crashes) {
        Ok(read) => read
            .flatten()
            .filter_map(|e| {
                let mtime = e.metadata().ok().and_then(|m| m.modified().ok())?;
                Some((e.path(), mtime))
            })
            .collect(),
        Err(_) => return, // crashes/ may simply not exist yet
    };
    dumps.sort_by_key(|(_, m)| *m);

    let now = SystemTime::now();
    if cfg.max_crash_age_days > 0 {
        let cutoff = Duration::from_secs(u64::from(cfg.max_crash_age_days) * 86_400);
        dumps.retain(|(p, mtime)| {
            let age = now.duration_since(*mtime).unwrap_or_default();
            if age > cutoff {
                if let Err(e) = std::fs::remove_file(p) {
                    tracing::warn!("cleanup: remove {p:?} failed: {e}");
                }
                false
            } else {
                true
            }
        });
    }

    while dumps.len() > cfg.max_crash_dumps {
        let (path, _) = dumps.remove(0);
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("cleanup: remove {path:?} failed: {e}");
        }
    }
}

fn crash_dir_from(log_dir: &Path) -> PathBuf {
    // Prefer the canonical resolved crash_dir() but fall back to a
    // join when the caller passed a custom dir (tests).
    let canonical = crash_dir();
    if canonical.parent() == Some(log_dir) {
        canonical
    } else {
        log_dir.join("crashes")
    }
}
