use super::*;
use std::path::{Path, PathBuf};

fn absolute_path(file: &str) -> PathBuf {
    #[cfg(windows)]
    let root = Path::new(r"C:\tmp");
    #[cfg(not(windows))]
    let root = Path::new("/tmp");
    root.join(file)
}

#[test]
fn classifies_shell_basenames_across_platform_path_syntax() {
    for program in ["/bin/sh", "/usr/bin/BASH", r"C:\tools\zsh.exe", "dash", "KSH.EXE"] {
        assert_eq!(classify_shell(program), ShellDialect::Posix, "{program}");
    }
    for program in ["pwsh", "PowerShell.EXE", r"C:\Program Files\PowerShell\pwsh.exe"] {
        assert_eq!(classify_shell(program), ShellDialect::PowerShell, "{program}");
    }
    assert_eq!(classify_shell(r"C:\Windows\System32\CMD.EXE"), ShellDialect::Cmd);
    assert_eq!(classify_shell("/usr/bin/perl"), ShellDialect::Unknown);
}

#[test]
fn powershell_quoting_doubles_apostrophes() {
    assert_eq!(shell_quote_powershell("a'b"), "'a''b'");
    assert_eq!(shell_quote_powershell(""), "''");
}

#[test]
fn formats_only_the_supported_shell_and_extension_matrix() {
    let posix_script = absolute_path("a b's.sh");
    let posix_command = absolute_path("run.command");
    let posix_tool = absolute_path("run.tool");
    let powershell_script = absolute_path("a b's.ps1");
    let powershell_cmd = absolute_path("run.CMD");
    let cmd_script = absolute_path("run.bat");

    assert_eq!(
        format_script_draft(ShellDialect::Posix, &posix_script),
        Ok(format!("sh {}", shell_quote_posix(posix_script.to_str().unwrap())))
    );
    assert_eq!(
        format_script_draft(ShellDialect::Posix, &posix_command),
        Ok(format!("sh {}", shell_quote_posix(posix_command.to_str().unwrap())))
    );
    assert_eq!(
        format_script_draft(ShellDialect::Posix, &posix_tool),
        Ok(format!("sh {}", shell_quote_posix(posix_tool.to_str().unwrap())))
    );
    assert_eq!(
        format_script_draft(ShellDialect::PowerShell, &powershell_script),
        Ok(format!("& {}", shell_quote_powershell(powershell_script.to_str().unwrap())))
    );
    assert_eq!(
        format_script_draft(ShellDialect::PowerShell, &powershell_cmd),
        Ok(format!("& {}", shell_quote_powershell(powershell_cmd.to_str().unwrap())))
    );
    assert_eq!(
        format_script_draft(ShellDialect::Cmd, &cmd_script),
        Ok(format!("\"{}\"", cmd_script.to_str().unwrap()))
    );

    for (dialect, file) in [
        (ShellDialect::Posix, "run.ps1"),
        (ShellDialect::PowerShell, "run.sh"),
        (ShellDialect::Cmd, "run.ps1"),
        (ShellDialect::Unknown, "run.sh"),
    ] {
        let path = absolute_path(file);
        assert_eq!(
            format_script_draft(dialect, &path),
            Err(DraftRejection::UnsupportedPair),
            "{dialect:?} {}",
            path.display()
        );
    }
}

#[test]
fn rejects_relative_control_and_cmd_expansion_paths() {
    assert_eq!(
        format_script_draft(ShellDialect::Posix, Path::new("run.sh")),
        Err(DraftRejection::NonAbsolutePath)
    );
    assert_eq!(
        format_script_draft(ShellDialect::Posix, &absolute_path("run\n.sh")),
        Err(DraftRejection::ControlCharacter)
    );
    for file in ["%TEMP%.cmd", "wow!.bat", "a\"b.cmd"] {
        let path = absolute_path(file);
        assert_eq!(
            format_script_draft(ShellDialect::Cmd, &path),
            Err(DraftRejection::CmdUnsafeCharacter),
            "{}",
            path.display()
        );
    }
}

#[test]
fn every_successful_draft_is_free_of_control_characters() {
    for (dialect, file) in [
        (ShellDialect::Posix, "run.sh"),
        (ShellDialect::PowerShell, "run.ps1"),
        (ShellDialect::Cmd, "run.cmd"),
    ] {
        let draft = format_script_draft(dialect, &absolute_path(file)).unwrap();
        assert!(!draft.chars().any(char::is_control), "{draft:?}");
    }
}

#[cfg(unix)]
#[test]
fn rejects_non_unicode_paths_without_lossy_conversion() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let path = Path::new(OsStr::from_bytes(b"/tmp/invalid-\xff.sh"));
    assert_eq!(format_script_draft(ShellDialect::Posix, path), Err(DraftRejection::NonUnicodePath));
}
