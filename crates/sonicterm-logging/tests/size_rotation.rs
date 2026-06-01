//! Size-based rotation: prove `max_file_size_mb` actually evicts the
//! active log when it exceeds the budget. Regression-guard for the
//! Haiku finding on PR #222 (the cleanup pass previously ignored
//! `max_file_size_mb` entirely — retention was only count + age).

use std::fs;

use sonicterm_logging::{cleanup_old_files, LoggingConfig};
use tempfile::tempdir;

#[test]
fn size_rotation_renames_oversized_active_log() {
    let dir = tempdir().unwrap();
    // tracing-appender's daily rotation produces names like
    // sonic.log.YYYY-MM-DD, so the "active" file is the newest
    // file matching the rotated prefix.
    let active = dir.path().join("sonic.log.2026-05-27");
    // Write 1.5 MiB to exceed a 1 MiB cap.
    let payload = vec![b'x'; 1_572_864];
    fs::write(&active, &payload).unwrap();
    assert_eq!(fs::metadata(&active).unwrap().len(), 1_572_864);

    let cfg = LoggingConfig {
        max_file_size_mb: 1,
        max_rotated_files: 100, // don't let count-cap eat the new rotated file
        max_age_days: 0,
        ..LoggingConfig::default()
    };
    cleanup_old_files(dir.path(), &cfg);

    // The original active file must have been renamed away.
    assert!(!active.exists(), "oversized active log was not size-rotated");
    // Some other rotated file (timestamped) must now exist.
    let rotated: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy().to_string();
            s.starts_with("sonic.log.")
        })
        .collect();
    assert_eq!(
        rotated.len(),
        1,
        "expected exactly 1 rotated file post-rotation, got {}",
        rotated.len()
    );
    let rotated_path = rotated[0].path();
    assert_ne!(rotated_path, active, "rotated file should have new name");
    // The rotated file carries the original bytes (rename preserves content).
    assert_eq!(fs::metadata(&rotated_path).unwrap().len(), 1_572_864);
}

#[test]
fn size_rotation_skipped_when_under_limit() {
    let dir = tempdir().unwrap();
    let active = dir.path().join("sonic.log.2026-05-27");
    fs::write(&active, b"small").unwrap();
    let cfg = LoggingConfig {
        max_file_size_mb: 1,
        max_rotated_files: 100,
        max_age_days: 0,
        ..LoggingConfig::default()
    };
    cleanup_old_files(dir.path(), &cfg);
    assert!(active.exists(), "small active log should not be rotated");
    assert_eq!(fs::metadata(&active).unwrap().len(), 5);
}

#[test]
fn size_rotation_disabled_when_zero() {
    let dir = tempdir().unwrap();
    let active = dir.path().join("sonic.log.2026-05-27");
    let big = vec![b'x'; 5 * 1024 * 1024];
    fs::write(&active, &big).unwrap();
    let cfg = LoggingConfig {
        max_file_size_mb: 0, // disabled
        max_rotated_files: 100,
        max_age_days: 0,
        ..LoggingConfig::default()
    };
    cleanup_old_files(dir.path(), &cfg);
    assert!(active.exists(), "size=0 should disable size rotation");
}
