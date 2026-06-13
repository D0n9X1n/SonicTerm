use sonicterm_logging::{LoggingConfig, DEFAULT_FILTER};

#[test]
fn exports_default_filter_and_config() {
    assert!(DEFAULT_FILTER.contains("sonicterm=warn"));
    assert_eq!(LoggingConfig::default().max_rotated_files, 3);
}
