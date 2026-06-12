
use super::*;

#[test]
fn fallback_log_dir_lives_under_dot_sonicterm() {
    let dir = resolve_log_dir();
    assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some("logs"));
    assert_eq!(
        dir.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()),
        Some(".sonicterm")
    );
}
