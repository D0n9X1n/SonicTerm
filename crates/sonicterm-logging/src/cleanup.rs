//! Retention enforcement for logs, crash dumps, and breadcrumbs.
//!
//! Cleanup is **fail-soft**: every filesystem error is logged at WARN
//! and swallowed so a hostile log directory cannot crash the app at
//! startup.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::config::LoggingConfig;
use crate::path::{crash_dir, log_file_name};
use crate::sinks::ROTATED_PREFIX;

/// Run a cleanup pass over `log_dir` and its artifact subdirectories.
///
/// Rotated logs remain bounded by file size, count, and age. Crash dumps and
/// per-session breadcrumb files are each bounded independently by count, age,
/// and aggregate bytes, evicting the oldest artifacts first. A zero age or
/// aggregate-byte value disables that axis; count remains authoritative.
///
/// The active `sonicterm.log` is never *deleted*, only renamed when it exceeds
/// the size cap.
pub fn cleanup_old_files(log_dir: &Path, cfg: &LoggingConfig) {
    cleanup_log_files(log_dir, cfg);
    cleanup_artifacts(log_dir, cfg);
}

/// Rotate and retain log files before the appender opens its active file.
pub fn cleanup_log_files(log_dir: &Path, cfg: &LoggingConfig) {
    enforce_size_rotation(log_dir, cfg);
    enforce_rotated_logs(log_dir, cfg);
}

/// Retain crash and breadcrumb artifacts after prior-session reporting.
pub fn cleanup_artifacts(log_dir: &Path, cfg: &LoggingConfig) {
    enforce_crash_dumps(log_dir, cfg);
    enforce_breadcrumbs(log_dir, cfg);
}

/// Spawn `cleanup_old_files` on a background thread.
pub fn cleanup_old_files_async(log_dir: PathBuf, cfg: &LoggingConfig) {
    let cfg = cfg.clone();
    std::thread::Builder::new()
        .name("sonicterm-logging-cleanup".to_string())
        .spawn(move || cleanup_old_files(&log_dir, &cfg))
        .map(|_| ())
        .unwrap_or_else(|e| tracing::warn!("failed to spawn cleanup thread: {e}"));
}

/// Aggressive cleanup invoked from the Help → Clear Old Logs menu
/// item: removes **every** rotated log file (i.e., every file whose
/// name starts with `sonicterm.log.` *except* the most recent one — the
/// active file `tracing-appender` is writing to) and **every** crash
/// dump. Returns a `(files_removed, bytes_removed)` pair for the UI
/// toast.
pub fn clear_all_rotated(log_dir: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let active = active_log(log_dir);
    if let Ok(read) = std::fs::read_dir(log_dir) {
        // When: read_dir enumerates log_dir; an unreadable directory leaves
        // nothing to remove and the pass reports zero rather than failing.
        for entry in read.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                // When: name is not UTF-8, so to_str yields nothing to match
                // against the rotation prefix.
                continue;
            };
            if name_str == log_file_name() {
                // When: name_str is the live log_file_name, which this pass
                // never deletes — only its rotated siblings are removable.
                continue;
            }
            if !name_str.starts_with(ROTATED_PREFIX) {
                // When: name_str lacks ROTATED_PREFIX, so it is another
                // writer's file sharing the directory and not ours to delete.
                continue;
            }
            let path = entry.path();
            if Some(&path) == active.as_ref() {
                // When: path is the active file the appender still holds open;
                // removing it would cut the running session's log.
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
/// bare `sonicterm.log` — every file is named `sonicterm.log.YYYY-MM-DD`. The
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
                // When: name_str is neither a ROTATED_PREFIX file nor
                // log_file_name, so this appender never wrote it.
                return None;
            }
            let mtime = e.metadata().ok().and_then(|m| m.modified().ok())?;
            Some((e.path(), mtime))
        })
        .collect();
    candidates.sort_by_key(|(_, m)| *m);
    candidates.pop().map(|(p, _)| p)
}

/// If the active log file is larger than `cfg.max_file_size_mb` MiB,
/// rename it to `sonicterm.log.<unix-seconds>` so the next write opens a
/// fresh file. `max_file_size_mb = 0` disables this check.
///
/// Rationale: `tracing-appender::rolling::daily` only rotates on the
/// day boundary, so a single chatty day can blow past the per-file
/// size budget that `[logging]` advertises. The rename here is a
/// best-effort second axis — when it fires the actively-open file
/// handle inside the appender keeps writing to the inode under its
/// new name; the next `daily` boundary then opens a fresh
/// `sonicterm.log.YYYY-MM-DD` and subsequent cleanups evict the
/// timestamp-suffixed file via [`enforce_rotated_logs`].
fn enforce_size_rotation(log_dir: &Path, cfg: &LoggingConfig) {
    if cfg.max_file_size_mb == 0 {
        // When: max_file_size_mb is zero, the size axis is switched off and the
        // daily boundary plus the count cap are all that bound the logs.
        return;
    }
    let limit_bytes = cfg.max_file_size_mb.saturating_mul(1024 * 1024);
    let Some(active) = active_log(log_dir) else {
        // When: active_log finds no candidate, the directory holds no log file
        // to rotate.
        return;
    };
    let Ok(meta) = std::fs::metadata(&active) else {
        // When: metadata on active fails, its size is unknown, and rotating on
        // a guess would rename a file that is still within budget.
        return;
    };
    if meta.len() <= limit_bytes {
        // When: meta reports at most limit_bytes, so the active file is inside
        // its budget and must keep its name.
        return;
    }
    let ts =
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut target = log_dir.join(format!("{}{ts}", crate::sinks::ROTATED_PREFIX));
    // Collision guard — if the same second already produced a rotated
    // file, append a monotonic counter so we don't clobber it.
    let mut bump = 0u32;
    while target.exists() {
        bump += 1;
        target = log_dir.join(format!("{}{ts}-{bump}", crate::sinks::ROTATED_PREFIX));
    }
    match std::fs::rename(&active, &target) {
        Ok(()) => {
            // On platforms where the appender holds the file open,
            // truncate the (newly-recreated-on-next-write) path so
            // the next write starts from zero. We don't pre-create
            // the file: `tracing-appender` will open it lazily.
            tracing::info!(
                from = %active.display(),
                to = %target.display(),
                size = meta.len(),
                "size-rotated active log"
            );
        }
        Err(e) => tracing::warn!("cleanup: size-rotate {active:?} -> {target:?} failed: {e}"),
    }
}

fn enforce_rotated_logs(log_dir: &Path, cfg: &LoggingConfig) {
    let active = active_log(log_dir);
    // When: read_dir on log_dir either enumerates the rotated files or fails,
    // in which case there is nothing to enforce and the pass gives up.
    let mut rotated: Vec<(PathBuf, SystemTime)> = match std::fs::read_dir(log_dir) {
        Ok(read) => read
            .flatten()
            .filter_map(|e| {
                let name = e.file_name();
                let name_str = name.to_str()?;
                if name_str == log_file_name() {
                    // When: name_str is the live log_file_name, which retention
                    // never evicts.
                    return None;
                }
                if !name_str.starts_with(ROTATED_PREFIX) {
                    // When: name_str lacks ROTATED_PREFIX, so it belongs to
                    // another writer sharing the directory.
                    return None;
                }
                let path = e.path();
                if Some(&path) == active.as_ref() {
                    // When: path is the active file the appender holds open, so
                    // the count and age axes must not consider it.
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
        // When: max_age_days is above zero, the age axis is active and evicts
        // before the count cap; zero disables it and leaves count authoritative.
        let cutoff = Duration::from_secs(u64::from(cfg.max_age_days) * 86_400);
        rotated.retain(|(p, mtime)| {
            let age = now.duration_since(*mtime).unwrap_or_default();
            if age > cutoff {
                if let Err(e) = std::fs::remove_file(p) {
                    tracing::warn!("cleanup: remove {p:?} failed: {e}");
                }
                false
            } else {
                // When: age is within cutoff, the file is young enough to keep
                // and stays for the count axis to judge.
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
    enforce_artifact_bounds(
        &crash_dir_from(log_dir),
        |_| true,
        cfg.max_crash_dumps,
        cfg.max_crash_age_days,
        cfg.max_crash_bytes,
    );
}

fn enforce_breadcrumbs(log_dir: &Path, cfg: &LoggingConfig) {
    enforce_artifact_bounds(
        &log_dir.join("breadcrumbs"),
        |name| {
            name.starts_with("breadcrumbs-") && (name.ends_with(".log") || name.ends_with(".tmp"))
        },
        cfg.max_breadcrumb_files,
        cfg.max_breadcrumb_age_days,
        cfg.max_breadcrumb_bytes,
    );
}

#[derive(Debug)]
struct Artifact {
    path: PathBuf,
    modified: SystemTime,
    bytes: u64,
}

fn enforce_artifact_bounds(
    dir: &Path,
    accepts: impl Fn(&str) -> bool,
    max_count: usize,
    max_age_days: u32,
    max_bytes: u64,
) {
    // When: read_dir on dir either enumerates the artifacts or fails, and a
    // missing artifact directory means there is nothing to bound.
    let mut artifacts: Vec<Artifact> = match std::fs::read_dir(dir) {
        Ok(read) => read
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_str()?;
                if !accepts(name) {
                    // When: accepts rejects name, so the entry belongs to
                    // another writer sharing the directory, not this class.
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                if !metadata.is_file() {
                    // When: metadata reports a non-file such as a
                    // subdirectory, which carries no artifact bytes to evict.
                    return None;
                }
                Some(Artifact {
                    path: entry.path(),
                    modified: metadata.modified().ok()?,
                    bytes: metadata.len(),
                })
            })
            .collect(),
        Err(_) => return,
    };
    artifacts.sort_by_key(|artifact| artifact.modified);

    if max_age_days > 0 {
        // When: max_age_days is above zero, the age axis is active and evicts
        // before the count and byte caps; zero switches that axis off.
        let now = SystemTime::now();
        let cutoff = Duration::from_secs(u64::from(max_age_days).saturating_mul(86_400));
        artifacts.retain(|artifact| {
            if now.duration_since(artifact.modified).unwrap_or_default() <= cutoff {
                // When: the artifact is within cutoff, so it is young enough to
                // keep and only the count and byte axes may evict it.
                return true;
            }
            remove_artifact(&artifact.path);
            false
        });
    }

    let mut aggregate_bytes =
        artifacts.iter().fold(0u64, |total, artifact| total.saturating_add(artifact.bytes));
    while artifacts.len() > max_count || (max_bytes > 0 && aggregate_bytes > max_bytes) {
        let artifact = artifacts.remove(0);
        aggregate_bytes = aggregate_bytes.saturating_sub(artifact.bytes);
        remove_artifact(&artifact.path);
    }
}

fn remove_artifact(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        tracing::warn!("cleanup: remove {path:?} failed: {error}");
    }
}

fn crash_dir_from(log_dir: &Path) -> PathBuf {
    // Prefer the canonical resolved crash_dir() but fall back to a
    // join when the caller passed a custom dir (tests).
    let canonical = crash_dir();
    if canonical.parent() == Some(log_dir) {
        canonical
    } else {
        // When: canonical does not sit under log_dir, the caller passed a
        // custom directory, so the crashes path is derived from that instead.
        log_dir.join("crashes")
    }
}
