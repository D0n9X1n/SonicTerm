use super::*;

#[test]
fn fallback_log_dir_lives_under_dot_sonicterm() {
    let dir = resolve_log_dir();
    assert_eq!(dir.file_name().and_then(|name| name.to_str()), Some("logs"));
    assert_eq!(
        dir.parent().and_then(|parent| parent.file_name()).and_then(|name| name.to_str()),
        Some(".sonicterm")
    );
}
