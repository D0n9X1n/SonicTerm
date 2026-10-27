//! Shared breadcrumb fixture limits and uniquely owned scratch directories.

use super::*;
use std::sync::atomic::AtomicU32;

pub(super) fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("sonicterm-breadcrumbs-{label}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

pub(super) fn limits() -> BreadcrumbLimits {
    let ring_capacity = 8;
    let required = required_file_bytes(ring_capacity).expect("valid test limit");
    BreadcrumbLimits {
        queue_capacity: 8,
        ring_capacity,
        max_file_bytes: required.max(MIN_FILE_BYTES),
        pressure_interval: Duration::from_secs(5),
        history_capacity: 48,
    }
}

pub(super) fn version() -> AppVersion {
    "1.2.3".parse().expect("valid version")
}
