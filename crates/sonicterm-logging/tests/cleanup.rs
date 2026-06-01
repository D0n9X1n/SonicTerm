//! Retention tests — fabricate rotated log files and crash dumps with
//! known mtimes, run cleanup, assert the exact survivors.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use sonicterm_logging::{cleanup_old_files, LoggingConfig};
use tempfile::tempdir;

fn touch(path: &Path, age_days: u32) {
    fs::write(path, b"x").unwrap();
    let mtime = SystemTime::now() - Duration::from_secs(u64::from(age_days) * 86_400 + 60);
    let ft = filetime::FileTime::from_system_time(mtime);
    filetime::set_file_mtime(path, ft).unwrap();
}

#[test]
fn cleanup_caps_rotated_file_count() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("sonicterm.log"), b"active").unwrap();
    for d in 1..=8 {
        touch(&dir.path().join(format!("sonicterm.log.2026-01-{d:02}")), d);
    }
    let cfg = LoggingConfig { max_rotated_files: 3, max_age_days: 0, ..LoggingConfig::default() };
    cleanup_old_files(dir.path(), &cfg);
    let rotated: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("sonicterm.log."))
        .collect();
    assert_eq!(rotated.len(), 3, "expected 3 rotated survivors, got {}", rotated.len());
    assert!(
        dir.path().join("sonicterm.log").exists(),
        "active sonicterm.log must never be removed"
    );
}

#[test]
fn cleanup_evicts_by_age() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("sonicterm.log"), b"active").unwrap();
    touch(&dir.path().join("sonicterm.log.2026-01-01"), 30);
    touch(&dir.path().join("sonicterm.log.2026-01-02"), 20);
    touch(&dir.path().join("sonicterm.log.2026-01-03"), 1);
    let cfg = LoggingConfig {
        max_rotated_files: 100, // age, not count, is the filter
        max_age_days: 14,
        ..LoggingConfig::default()
    };
    cleanup_old_files(dir.path(), &cfg);
    assert!(!dir.path().join("sonicterm.log.2026-01-01").exists());
    assert!(!dir.path().join("sonicterm.log.2026-01-02").exists());
    assert!(dir.path().join("sonicterm.log.2026-01-03").exists());
    assert!(dir.path().join("sonicterm.log").exists());
}

#[test]
fn cleanup_handles_empty_dir_without_panic() {
    let dir = tempdir().unwrap();
    let cfg = LoggingConfig::default();
    cleanup_old_files(dir.path(), &cfg);
    assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn cleanup_never_deletes_active_sonic_log() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("sonicterm.log"), b"hot").unwrap();
    let ft =
        filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(365 * 86_400));
    filetime::set_file_mtime(dir.path().join("sonicterm.log"), ft).unwrap();
    let cfg = LoggingConfig {
        max_rotated_files: 0,
        max_age_days: 1,
        max_crash_age_days: 1,
        ..LoggingConfig::default()
    };
    cleanup_old_files(dir.path(), &cfg);
    assert!(dir.path().join("sonicterm.log").exists(), "active log was wrongly deleted");
}

#[test]
fn cleanup_caps_crash_dump_count() {
    // Need SONIC_LOG_DIR to point at our tempdir so the helper that
    // resolves the crash subdir lands inside it.
    let dir = tempdir().unwrap();
    std::env::set_var("SONIC_LOG_DIR", dir.path());
    let crashes = dir.path().join("crashes");
    fs::create_dir_all(&crashes).unwrap();
    for d in 1..=7 {
        touch(&crashes.join(format!("crash-2026-01-{d:02}.log")), d);
    }
    let cfg = LoggingConfig {
        max_crash_dumps: 2,
        max_crash_age_days: 0,
        max_rotated_files: 100,
        max_age_days: 0,
        ..LoggingConfig::default()
    };
    cleanup_old_files(dir.path(), &cfg);
    let remaining: Vec<_> = fs::read_dir(&crashes).unwrap().flatten().collect();
    assert_eq!(remaining.len(), 2, "expected 2 crash survivors, got {}", remaining.len());
    std::env::remove_var("SONIC_LOG_DIR");
}
