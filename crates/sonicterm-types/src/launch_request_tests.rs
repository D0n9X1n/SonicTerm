use super::*;

/// A tab label and log name keep only the program's last path component, so a
/// launcher's directory never reaches a label or a log line.
#[test]
fn program_file_name_keeps_only_the_last_component() {
    let root = if cfg!(windows) { r"C:\tools\bin\htop.exe" } else { "/usr/bin/htop" };
    let launch = CommandLaunch { program: OsString::from(root), args: Vec::new() };
    let expected = if cfg!(windows) { "htop.exe" } else { "htop" };
    assert_eq!(launch.program_file_name(), expected);
}

/// Control characters in a program name cannot inject terminal or log control sequences.
#[test]
fn program_file_name_replaces_control_characters() {
    let launch = CommandLaunch { program: OsString::from("evil\u{1b}[2Jname"), args: Vec::new() };
    assert_eq!(launch.program_file_name(), "evil\u{FFFD}[2Jname");
    assert_eq!(printable_file_name(OsStr::new("")), "program");
}

/// The first tab's label prefers the explicit title, then the program, then `shell`.
#[test]
fn tab_title_prefers_title_then_program_then_shell() {
    let mut launch = TerminalLaunch::default();
    assert_eq!(launch.tab_title(), "shell");
    launch.command = Some(CommandLaunch { program: OsString::from("ls"), args: Vec::new() });
    assert_eq!(launch.tab_title(), "ls");
    launch.title = Some("Logs".to_string());
    assert_eq!(launch.tab_title(), "Logs");
}

/// Only exit code zero closes a pane; every other status, including unknown, holds it.
#[test]
fn only_code_zero_is_clean() {
    assert!(PaneExitStatus::Code(0).is_clean());
    assert!(!PaneExitStatus::Code(1).is_clean());
    assert!(!PaneExitStatus::Signal("SIGKILL".to_string()).is_clean());
    assert!(!PaneExitStatus::Unknown.is_clean());
}

/// The held-pane status line has one fixed wording per status kind.
#[test]
fn status_lines_name_the_exit_kind() {
    assert_eq!(PaneExitStatus::Code(1).status_line(), "[process exited with code 1]");
    assert_eq!(
        PaneExitStatus::Signal("SIGKILL".to_string()).status_line(),
        "[process killed by signal SIGKILL]"
    );
    assert_eq!(PaneExitStatus::Unknown.status_line(), "[process exit status unknown]");
    assert_eq!(
        PaneExitStatus::Signal("SIG\u{1b}X".to_string()).status_line(),
        "[process killed by signal SIGX]"
    );
}

/// Cwd sources log under stable values that the Logging page documents.
#[test]
fn cwd_sources_have_stable_log_values() {
    assert_eq!(CwdSource::Dir.as_str(), "--dir");
    assert_eq!(CwdSource::Process.as_str(), "process");
    assert_eq!(CwdSource::Home.as_str(), "home");
}

/// Non-UTF-8 program bytes survive in the request unchanged.
#[cfg(unix)]
#[test]
fn non_utf8_program_and_args_round_trip() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let raw = b"/tmp/prog-\xff".to_vec();
    let launch = CommandLaunch {
        program: OsString::from_vec(raw.clone()),
        args: vec![OsString::from_vec(b"\xfe".to_vec())],
    };
    assert_eq!(launch.program.as_bytes(), raw.as_slice());
    assert_eq!(launch.args[0].as_bytes(), b"\xfe");
    assert_eq!(launch.program_file_name(), "prog-\u{FFFD}");
}
