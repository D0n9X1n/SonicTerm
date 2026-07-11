//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{LoggingConfig, DEFAULT_FILTER};

#[test]
fn exports_default_filter_and_config() {
    assert!(DEFAULT_FILTER.contains("sonicterm=warn"));
    assert_eq!(LoggingConfig::default().max_rotated_files, 3);
}
