
use super::*;

#[test]
fn default_retention_cleans_after_two_days() {
    let cfg = LoggingConfig::default();
    assert_eq!(cfg.max_age_days, 2);
    assert_eq!(cfg.max_crash_age_days, 2);
}

#[test]
fn default_level_is_warn() {
    assert_eq!(LoggingConfig::default().level, LogLevel::Warn);
}
