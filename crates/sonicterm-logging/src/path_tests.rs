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

/// A non-empty `SONICTERM_LOG_DIR` replaces the whole log tree, so a diagnostic
/// run's logs never land in the user's `~/.sonicterm/logs`.
#[test]
fn log_dir_override_replaces_the_default_tree() {
    let dir = resolve_log_dir_with(|name| match name {
        LOG_DIR_ENV => Some("/scratch/logs".into()),
        "HOME" => Some("/home/user".into()),
        _ => None,
    });
    assert_eq!(dir, PathBuf::from("/scratch/logs"));
}

/// An empty override is treated as unset, so `SONICTERM_LOG_DIR=` cannot send
/// logs to the current working directory.
#[test]
fn empty_log_dir_override_falls_back_to_home() {
    let dir = resolve_log_dir_with(|name| match name {
        LOG_DIR_ENV => Some("".into()),
        "HOME" => Some("/home/user".into()),
        _ => None,
    });
    assert_eq!(dir, PathBuf::from("/home/user/.sonicterm/logs"));
}

/// Windows has no `HOME`; `USERPROFILE` supplies the user directory instead.
#[test]
fn log_dir_uses_userprofile_without_home() {
    let dir = resolve_log_dir_with(|name| match name {
        "USERPROFILE" => Some("/profile".into()),
        _ => None,
    });
    assert_eq!(dir, PathBuf::from("/profile/.sonicterm/logs"));
}
