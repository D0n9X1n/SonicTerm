use super::*;

#[test]
fn explicit_child_cwd_overrides_home() {
    let explicit = std::env::temp_dir();
    let mut builder = CommandBuilder::new("sh");
    apply_child_cwd(&mut builder, Some(&explicit), Some("/home/fallback"));
    assert_eq!(builder.get_cwd().map(Path::new), Some(explicit.as_path()));
}

#[test]
fn child_cwd_uses_home_only_without_an_override() {
    let mut builder = CommandBuilder::new("sh");
    apply_child_cwd(&mut builder, None, Some("/home/fallback"));
    assert_eq!(builder.get_cwd().map(Path::new), Some(Path::new("/home/fallback")));
}

#[test]
fn child_cwd_remains_unset_without_override_or_home() {
    let mut builder = CommandBuilder::new("sh");
    apply_child_cwd(&mut builder, None, None);
    assert_eq!(builder.get_cwd(), None);
}

#[test]
fn unusable_explicit_child_cwd_falls_back_to_home() {
    let missing = std::env::temp_dir().join(format!(
        "sonicterm-missing-cwd-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&missing);
    let mut builder = CommandBuilder::new("sh");

    apply_child_cwd(&mut builder, Some(&missing), Some("/home/fallback"));

    assert_eq!(builder.get_cwd().map(Path::new), Some(Path::new("/home/fallback")));
}

/// A command whose program cannot be resolved fails with a typed error and starts no process or shell.
#[test]
fn spawn_command_failure_starts_nothing() {
    let cwd = std::env::temp_dir();
    let spawn = CommandSpawn {
        program: std::ffi::OsString::from("sonicterm-no-such-program-for-tests"),
        args: vec![std::ffi::OsString::from("secret-argument-value")],
        cwd,
    };
    let error = PtyHandle::spawn_command(&spawn, 80, 24, &ShellSpawnOpts::default())
        .err()
        .expect("missing program must fail");
    assert!(matches!(error, CommandSpawnError::NotFound { .. }), "{error:?}");
    assert!(!error.to_string().contains("secret-argument-value"));
}

/// A missing cwd fails the spawn instead of starting the program in a home directory.
#[test]
fn spawn_command_with_missing_cwd_fails() {
    let root = tempfile::tempdir().expect("tempdir");
    let gone = root.path().join("gone");
    #[cfg(unix)]
    let program = "/bin/sh";
    #[cfg(windows)]
    let program = "cmd.exe";
    let spawn = CommandSpawn { program: program.into(), args: Vec::new(), cwd: gone };
    let error = PtyHandle::spawn_command(&spawn, 80, 24, &ShellSpawnOpts::default())
        .err()
        .expect("missing cwd must fail");
    assert!(matches!(error, CommandSpawnError::InvalidCwd { .. }), "{error:?}");
}

/// A native spawn failure is mapped to a fixed reason; the lower layer's path text is not forwarded.
#[test]
fn spawn_errors_are_sanitized() {
    let native = anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "Unable to spawn /secret/dir/tool",
    ));
    let error = sanitize_spawn_error(std::ffi::OsStr::new("/secret/dir/tool"), &native);
    assert_eq!(error.to_string(), "cannot run tool: permission denied");
}
