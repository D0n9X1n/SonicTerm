use super::*;

/// Windows reads `x.bat.`, `x.bat ` and `x.bat::$DATA` as the batch file `x.bat`, so each is classified as batch.
#[test]
fn batch_names_cover_windows_spelling_variants() {
    for spelled in
        ["run.bat", "RUN.CMD", "run.bat.", "run.cmd  ", "run.bat::$DATA", r"C:\x\run.Bat"]
    {
        assert!(is_batch_name(OsStr::new(spelled)), "{spelled} is a batch file");
    }
    for spelled in ["run.exe", "run.batch", "bat", "run.com", r"C:\x.bat\run.exe"] {
        assert!(!is_batch_name(OsStr::new(spelled)), "{spelled} is not a batch file");
    }
}

/// Every error message names the file name and the rule, never a directory.
#[test]
fn error_messages_name_only_the_program_file() {
    let error =
        CommandSpawnError::NotFound { program: display_name(OsStr::new("/secret/dir/tool")) };
    assert_eq!(error.to_string(), "cannot run tool: no such program");
    let hint = CommandSpawnError::BatchScript { program: "build.bat".to_string() };
    assert!(hint.to_string().contains("cmd /c build.bat"));
}

/// An empty program is refused before any filesystem access.
#[test]
fn empty_program_is_refused() {
    let cwd = std::env::temp_dir();
    assert_eq!(resolve_program(OsStr::new(""), &cwd, None), Err(CommandSpawnError::EmptyProgram));
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write `contents` to `path` with `mode`.
    fn write_file(path: &Path, contents: &[u8], mode: u32) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(path, contents).expect("write file");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// A `#!` script with the execute bit.
    fn write_script(path: &Path) {
        write_file(path, b"#!/bin/sh\nexit 0\n", 0o755);
    }

    /// A program with a slash resolves against the launch cwd, even when a PATH entry has the same relative path.
    #[test]
    fn slash_program_resolves_against_cwd_not_path() {
        let root = tempfile::tempdir().expect("tempdir");
        let cwd = root.path().join("cwd");
        let on_path = root.path().join("onpath");
        write_script(&cwd.join("subdir/program"));
        write_script(&on_path.join("subdir/program"));
        let resolved =
            resolve_program(OsStr::new("subdir/program"), &cwd, Some(on_path.as_os_str())).unwrap();
        assert_eq!(resolved, cwd.join("subdir/program"));
    }

    /// Relative and empty PATH components are skipped, so a bare name never resolves against the cwd.
    #[test]
    fn relative_and_empty_path_entries_are_skipped() {
        let root = tempfile::tempdir().expect("tempdir");
        let cwd = root.path().join("cwd");
        write_script(&cwd.join("tool"));
        write_script(&cwd.join("rel/tool"));
        let path_var = OsString::from(":rel:.");
        let error = resolve_program(OsStr::new("tool"), &cwd, Some(&path_var)).unwrap_err();
        assert!(matches!(error, CommandSpawnError::NotFound { .. }), "{error:?}");

        let bin = root.path().join("bin");
        write_script(&bin.join("tool"));
        let mut mixed = OsString::from(":rel:");
        mixed.push(bin.as_os_str());
        assert_eq!(
            resolve_program(OsStr::new("tool"), &cwd, Some(&mixed)).unwrap(),
            bin.join("tool")
        );
    }

    /// A missing or deleted cwd fails, with no fallback to the home directory.
    #[test]
    fn missing_cwd_fails_without_fallback() {
        let root = tempfile::tempdir().expect("tempdir");
        let gone = root.path().join("gone");
        std::fs::create_dir(&gone).expect("mkdir");
        std::fs::remove_dir(&gone).expect("rmdir");
        let error = resolve_program(OsStr::new("/bin/sh"), &gone, None).unwrap_err();
        assert!(matches!(error, CommandSpawnError::InvalidCwd { .. }), "{error:?}");
        let relative = Path::new("relative/dir");
        let error = resolve_program(OsStr::new("/bin/sh"), relative, None).unwrap_err();
        assert!(matches!(error, CommandSpawnError::InvalidCwd { .. }), "{error:?}");
    }

    /// An executable text file without `#!` is refused, so libc's ENOEXEC path never hands it to `/bin/sh`.
    #[test]
    fn executable_text_without_interpreter_line_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("plain");
        write_file(&path, b"echo pwned\n", 0o755);
        let error = resolve_program(path.as_os_str(), root.path(), None).unwrap_err();
        assert!(matches!(error, CommandSpawnError::NoInterpreterLine { .. }), "{error:?}");
        let empty = root.path().join("empty");
        write_file(&empty, b"", 0o755);
        let error = resolve_program(empty.as_os_str(), root.path(), None).unwrap_err();
        assert!(matches!(error, CommandSpawnError::NoInterpreterLine { .. }), "{error:?}");
    }

    /// A `#!` script and a native binary are both admitted.
    #[test]
    fn interpreter_script_and_native_binary_resolve() {
        let root = tempfile::tempdir().expect("tempdir");
        let script = root.path().join("script");
        write_script(&script);
        assert_eq!(resolve_program(script.as_os_str(), root.path(), None).unwrap(), script);
        let native = PathBuf::from("/bin/sh");
        assert_eq!(resolve_program(native.as_os_str(), root.path(), None).unwrap(), native);
    }

    /// A directory and a non-executable file are refused with their own rules.
    #[test]
    fn directory_and_non_executable_are_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let folder = root.path().join("folder");
        std::fs::create_dir(&folder).expect("mkdir");
        let error = resolve_program(folder.as_os_str(), root.path(), None).unwrap_err();
        assert!(matches!(error, CommandSpawnError::IsDirectory { .. }), "{error:?}");
        let data = root.path().join("data");
        write_file(&data, b"#!/bin/sh\n", 0o644);
        let error = resolve_program(data.as_os_str(), root.path(), None).unwrap_err();
        assert!(matches!(error, CommandSpawnError::NotExecutable { .. }), "{error:?}");
    }

    /// A PATH search skips a non-executable candidate and takes a later executable one, as execvp does.
    #[test]
    fn path_search_continues_past_a_failing_candidate() {
        let root = tempfile::tempdir().expect("tempdir");
        let first = root.path().join("first");
        let second = root.path().join("second");
        write_file(&first.join("tool"), b"#!/bin/sh\n", 0o644);
        write_script(&second.join("tool"));
        let path_var = std::env::join_paths([&first, &second]).expect("join");
        let resolved = resolve_program(OsStr::new("tool"), root.path(), Some(&path_var)).unwrap();
        assert_eq!(resolved, second.join("tool"));
    }

    /// Only `#!` and this platform's executable magic count as runnable headers.
    #[test]
    fn known_headers_are_shebang_and_native_magic() {
        assert!(platform::is_known_header(b"#!/bin/sh"));
        assert!(!platform::is_known_header(b"echo"));
        assert!(!platform::is_known_header(b""));
        #[cfg(target_os = "macos")]
        assert!(platform::is_known_header(&[0xcf, 0xfa, 0xed, 0xfe]));
        #[cfg(not(target_os = "macos"))]
        assert!(platform::is_known_header(b"\x7fELF"));
    }
}

#[cfg(windows)]
mod windows {
    use super::*;

    /// Create an empty file at `path`.
    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(path, b"MZ").expect("write file");
    }

    /// Batch files are refused with the `cmd /c` hint, whether named directly or found by a search.
    #[test]
    fn batch_files_are_refused_with_the_cmd_hint() {
        let root = tempfile::tempdir().expect("tempdir");
        touch(&root.path().join("build.bat"));
        touch(&root.path().join("build.cmd"));
        let error =
            resolve_program(OsStr::new("build.bat"), root.path(), Some(root.path().as_os_str()))
                .unwrap_err();
        assert!(matches!(error, CommandSpawnError::BatchScript { .. }), "{error:?}");
        assert!(error.to_string().contains("cmd /c"));
        let explicit = root.path().join("build.cmd");
        let error = resolve_program(explicit.as_os_str(), root.path(), None).unwrap_err();
        assert!(matches!(error, CommandSpawnError::BatchScript { .. }), "{error:?}");
    }

    /// PATHEXT entries other than `.exe` and `.com` are never tried, so `build` cannot find `build.bat`.
    #[test]
    fn only_exe_and_com_extensions_are_tried() {
        let root = tempfile::tempdir().expect("tempdir");
        touch(&root.path().join("build.bat"));
        touch(&root.path().join("build.ps1"));
        let error =
            resolve_program(OsStr::new("build"), root.path(), Some(root.path().as_os_str()))
                .unwrap_err();
        assert!(matches!(error, CommandSpawnError::NotFound { .. }), "{error:?}");
        touch(&root.path().join("tool.com"));
        let resolved =
            resolve_program(OsStr::new("tool"), root.path(), Some(root.path().as_os_str()))
                .unwrap();
        assert_eq!(resolved, root.path().join("tool.com"));
    }

    /// A program not found is an error, never passed through to `CreateProcessW` unchanged.
    #[test]
    fn missing_program_is_an_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let error =
            resolve_program(OsStr::new("no-such-tool"), root.path(), Some(root.path().as_os_str()))
                .unwrap_err();
        assert!(matches!(error, CommandSpawnError::NotFound { .. }), "{error:?}");
    }

    /// Drive-relative and root-relative programs depend on per-drive state and are refused.
    #[test]
    fn drive_and_root_relative_programs_are_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        for spelled in [r"C:tool.exe", r"\tool.exe"] {
            let error = resolve_program(OsStr::new(spelled), root.path(), None).unwrap_err();
            assert!(matches!(error, CommandSpawnError::NotFound { .. }), "{spelled}: {error:?}");
        }
    }

    /// Relative and empty PATH components are skipped on Windows too.
    #[test]
    fn relative_path_entries_are_skipped() {
        let root = tempfile::tempdir().expect("tempdir");
        touch(&root.path().join("rel").join("tool.exe"));
        let error = resolve_program(OsStr::new("tool"), root.path(), Some(OsStr::new("rel;;.")))
            .unwrap_err();
        assert!(matches!(error, CommandSpawnError::NotFound { .. }), "{error:?}");
    }
}
