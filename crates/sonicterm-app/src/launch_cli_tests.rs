use super::*;

/// Parse `args` (binary name prepended) with no process cwd.
fn parse(dialect: CliDialect, args: &[&str]) -> Result<LaunchCli, UsageError> {
    let mut all = vec!["sonicterm"];
    all.extend_from_slice(args);
    parse_launch_args(all, dialect, None)
}

/// Parse under the Linux dialect.
fn linux(args: &[&str]) -> Result<LaunchCli, UsageError> {
    parse(CliDialect::Linux, args)
}

/// The terminal options of a successful parse.
fn terminal(result: Result<LaunchCli, UsageError>) -> TerminalCli {
    match result {
        Ok(LaunchCli::Terminal(cli)) => cli,
        other => panic!("expected a terminal launch, got {other:?}"),
    }
}

/// The rule of a failed parse.
fn rule(result: Result<LaunchCli, UsageError>) -> UsageRule {
    result.expect_err("expected a usage error").rule
}

/// No arguments opens an ordinary shell window.
#[test]
fn no_arguments_is_a_plain_terminal() {
    assert_eq!(terminal(linux(&[])), TerminalCli::default());
}

/// Each delimiter stops option parsing, and every later token reaches the program.
#[test]
fn each_delimiter_stops_option_parsing() {
    for delimiter in ["-e", "--command", "--"] {
        let cli = terminal(linux(&[delimiter, "ls", "-T", "x", "--hold", "--runtime-smoke"]));
        let command = cli.command.expect("command");
        assert_eq!(command.program, "ls");
        assert_eq!(command.args, ["-T", "x", "--hold", "--runtime-smoke"]);
        assert_eq!(cli.title, None, "{delimiter}: -T after the delimiter is the child's");
        assert!(!cli.hold, "{delimiter}: --hold after the delimiter is the child's");
    }
}

/// An option value is consumed verbatim even when it looks like the delimiter.
#[test]
fn title_value_may_start_with_a_dash() {
    let cli = terminal(linux(&["-T", "-e"]));
    assert_eq!(cli.title.as_deref(), Some("-e"));
    assert!(cli.command.is_none());
    let joined = terminal(linux(&["--title=Logs", "-e", "ls"]));
    assert_eq!(joined.title.as_deref(), Some("Logs"));
}

/// `-e` takes one program name; a quoted command line is a literal program name, never split.
#[test]
fn program_with_spaces_is_one_name() {
    let command = terminal(linux(&["-e", "cmd args"])).command.expect("command");
    assert_eq!(command.program, "cmd args");
    assert!(command.args.is_empty());
}

/// A bare delimiter and an empty program are usage errors, never a shell.
#[test]
fn missing_or_empty_program_is_an_error() {
    assert_eq!(rule(linux(&["-e"])), UsageRule::MissingProgram);
    assert_eq!(rule(linux(&["--"])), UsageRule::MissingProgram);
    let error = linux(&["-e", ""]).unwrap_err();
    assert_eq!(error, UsageError { position: 2, rule: UsageRule::EmptyProgram });
}

/// Every grammar violation is a usage error naming its position.
#[test]
fn grammar_violations_are_usage_errors() {
    assert_eq!(
        linux(&["file.sh"]).unwrap_err(),
        UsageError { position: 1, rule: UsageRule::Positional }
    );
    assert_eq!(rule(linux(&["--title", "x"])), UsageRule::JoinedValueRequired);
    assert_eq!(rule(linux(&["--dir", "/tmp"])), UsageRule::JoinedValueRequired);
    assert_eq!(
        linux(&["-T", "a", "-T", "b"]).unwrap_err(),
        UsageError { position: 3, rule: UsageRule::Repeated }
    );
    assert_eq!(rule(linux(&["-T", "a", "--title=b"])), UsageRule::Repeated);
    assert_eq!(rule(linux(&["--dir="])), UsageRule::EmptyDir);
    assert_eq!(rule(linux(&["--hold", "--hold"])), UsageRule::Repeated);
    assert_eq!(rule(linux(&["--open-script", "x.sh", "-e", "ls"])), UsageRule::Conflict);
    assert_eq!(rule(linux(&["-T", "x", "--open-script", "x.sh"])), UsageRule::Conflict);
    assert_eq!(
        rule(linux(&["--open-script", "a.sh", "--open-script", "b.sh"])),
        UsageRule::Repeated
    );
    assert_eq!(
        linux(&["--bogus"]).unwrap_err(),
        UsageError { position: 1, rule: UsageRule::UnknownOption }
    );
    assert_eq!(rule(linux(&["-T"])), UsageRule::MissingValue);
    assert_eq!(rule(linux(&["--open-script"])), UsageRule::MissingValue);
}

/// `--runtime-smoke` runs only alone; any companion before the delimiter is a conflict.
#[test]
fn runtime_smoke_is_exclusive() {
    assert_eq!(linux(&["--runtime-smoke"]), Ok(LaunchCli::RuntimeSmoke));
    for companion in [&["--hold"][..], &["-T", "x"], &["-e", "ls"], &["--open-script", "x"]] {
        let mut args = vec!["--runtime-smoke"];
        args.extend_from_slice(companion);
        assert_eq!(rule(linux(&args)), UsageRule::Conflict, "{args:?}");
    }
    assert_eq!(rule(linux(&["--runtime-smoke", "--runtime-smoke"])), UsageRule::Repeated);
    assert_eq!(rule(linux(&["--hold", "--runtime-smoke"])), UsageRule::Conflict);
}

/// `--hold` without a command marks the initial shell.
#[test]
fn hold_without_command_marks_the_shell() {
    let cli = terminal(linux(&["--hold"]));
    assert!(cli.hold);
    assert!(cli.command.is_none());
}

/// `--help` returns usage even when other arguments follow.
#[test]
fn help_wins() {
    assert_eq!(linux(&["--help"]), Ok(LaunchCli::Help));
    assert_eq!(linux(&["--hold", "--help", "--bogus"]), Ok(LaunchCli::Help));
    assert!(usage(CliDialect::Linux).contains("-e \"cmd args\" names a program"));
    assert!(usage(CliDialect::Windows).contains("cmd /c"));
}

/// `--open-script` keeps its path as given, for the binary to resolve.
#[test]
fn open_script_keeps_its_path() {
    assert_eq!(
        linux(&["--open-script", "./x.sh"]),
        Ok(LaunchCli::OpenScript(PathBuf::from("./x.sh")))
    );
    assert_eq!(linux(&["--open-script", "-e"]), Ok(LaunchCli::OpenScript(PathBuf::from("-e"))));
}

/// Windows-only options are unknown to Linux.
#[test]
fn windows_options_are_unknown_on_linux() {
    assert_eq!(rule(linux(&["--refresh-shell-associations"])), UsageRule::UnknownOption);
    assert_eq!(rule(linux(&["--tear-out-payload", "{}"])), UsageRule::UnknownOption);
}

/// Windows keeps refresh alone, the tear-out last-value rule, and now rejects unknown options.
#[test]
fn windows_private_options() {
    let windows = |args: &[&str]| parse(CliDialect::Windows, args);
    assert_eq!(windows(&["--refresh-shell-associations"]), Ok(LaunchCli::RefreshShellAssociations));
    assert_eq!(rule(windows(&["--refresh-shell-associations", "--hold"])), UsageRule::Conflict);
    assert_eq!(
        windows(&["--tear-out-payload", "first", "--tear-out-payload", "last"]),
        Ok(LaunchCli::TearOut(OsString::from("last")))
    );
    assert_eq!(
        rule(windows(&["--tear-out-payload", "x", "--open-script", "y"])),
        UsageRule::Conflict
    );
    assert_eq!(rule(windows(&["--tear-out-payload", "x", "-T", "y"])), UsageRule::Conflict);
    assert_eq!(rule(windows(&["--unknown", "value"])), UsageRule::UnknownOption);
}

/// `--dir` resolves a relative path against the process cwd and requires a directory.
#[test]
fn dir_resolves_against_the_process_cwd() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join("logs")).expect("mkdir");
    std::fs::write(root.path().join("file"), b"x").expect("write");
    let parse_in = |args: &[&str]| {
        let mut all = vec!["sonicterm"];
        all.extend_from_slice(args);
        parse_launch_args(all, CliDialect::Linux, Some(root.path()))
    };
    assert_eq!(terminal(parse_in(&["--dir=logs"])).dir, Some(root.path().join("logs")));
    let absolute = format!("--working-directory={}", root.path().join("logs").display());
    assert_eq!(terminal(parse_in(&[&absolute])).dir, Some(root.path().join("logs")));
    assert_eq!(rule(parse_in(&["--dir=file"])), UsageRule::DirNotDirectory);
    assert_eq!(rule(parse_in(&["--dir=missing"])), UsageRule::DirNotDirectory);
    assert_eq!(rule(linux(&["--dir=logs"])), UsageRule::DirNotDirectory, "no process cwd");
    assert_eq!(rule(parse_in(&["--dir=logs", "--working-directory=logs"])), UsageRule::Repeated);
}

/// Usage errors never print or log the argument's value.
#[test]
fn usage_error_text_has_no_values() {
    let error = linux(&["--secret-value-option"]).unwrap_err();
    assert_eq!(error.to_string(), "argument 1: unknown option");
}

/// Linux starts a plain shell in the process cwd; Windows does so only for commands.
#[test]
fn default_cwd_follows_the_dialect() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("mkdir");
    let cwd = root.path().to_path_buf();
    let shell = TerminalCli::default();
    let linux_shell =
        terminal_launch(shell.clone(), CliDialect::Linux, Some(cwd.clone()), Some(home.clone()));
    assert_eq!(
        (linux_shell.cwd, linux_shell.cwd_source),
        (Some(cwd.clone()), Some(CwdSource::Process))
    );
    let windows_shell =
        terminal_launch(shell, CliDialect::Windows, Some(cwd.clone()), Some(home.clone()));
    assert_eq!((windows_shell.cwd, windows_shell.cwd_source), (None, None));
    let command = TerminalCli {
        command: Some(CommandLaunch { program: OsString::from("ls"), args: Vec::new() }),
        ..TerminalCli::default()
    };
    let windows_command = terminal_launch(
        command.clone(),
        CliDialect::Windows,
        Some(cwd.clone()),
        Some(home.clone()),
    );
    assert_eq!(windows_command.cwd_source, Some(CwdSource::Process));
    let missing = root.path().join("gone");
    let fallback = terminal_launch(command, CliDialect::Linux, Some(missing), Some(home.clone()));
    assert_eq!((fallback.cwd, fallback.cwd_source), (Some(home), Some(CwdSource::Home)));
}

/// Non-UTF-8 program, arguments and `--dir` survive parsing byte-for-byte; a non-UTF-8 title is refused.
///
/// APFS refuses a non-UTF-8 directory name, so the `--dir` half runs only where the filesystem accepts one.
#[cfg(unix)]
#[test]
fn non_utf8_values_round_trip() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let root = tempfile::tempdir().expect("tempdir");
    let dir_name = OsString::from_vec(b"dir-\xff".to_vec());
    let dir_supported = std::fs::create_dir(root.path().join(&dir_name)).is_ok();
    let mut args = vec![OsString::from("sonicterm")];
    if dir_supported {
        let mut dir_arg = OsString::from("--dir=");
        dir_arg.push(&dir_name);
        args.push(dir_arg);
    }
    args.extend([
        OsString::from("-e"),
        OsString::from_vec(b"prog-\xfe".to_vec()),
        OsString::from_vec(b"arg-\xfd".to_vec()),
    ]);
    let cli = terminal(parse_launch_args(args, CliDialect::Linux, Some(root.path())));
    if dir_supported {
        assert_eq!(cli.dir, Some(root.path().join(&dir_name)));
    }
    let command = cli.command.expect("command");
    assert_eq!(command.program.as_bytes(), b"prog-\xfe");
    assert_eq!(command.args[0].as_bytes(), b"arg-\xfd");
    let title = vec![
        OsString::from("sonicterm"),
        OsString::from("-T"),
        OsString::from_vec(b"\xff".to_vec()),
    ];
    let error = parse_launch_args(title, CliDialect::Linux, None).unwrap_err();
    assert_eq!(error, UsageError { position: 2, rule: UsageRule::NonUnicodeTitle });
}

/// Unpaired surrogates in the program and arguments survive parsing unit-for-unit; such a title is refused.
#[cfg(windows)]
#[test]
fn unpaired_surrogates_round_trip() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let program = OsString::from_wide(&[0x0070, 0xD800]);
    let arg = OsString::from_wide(&[0xDC00, 0x0061]);
    let args =
        vec![OsString::from("sonicterm"), OsString::from("--"), program.clone(), arg.clone()];
    let command =
        terminal(parse_launch_args(args, CliDialect::Windows, None)).command.expect("command");
    assert_eq!(
        command.program.encode_wide().collect::<Vec<_>>(),
        program.encode_wide().collect::<Vec<_>>()
    );
    assert_eq!(
        command.args[0].encode_wide().collect::<Vec<_>>(),
        arg.encode_wide().collect::<Vec<_>>()
    );
    let mut title_arg = OsString::from("--title=");
    title_arg.push(OsString::from_wide(&[0xD800]));
    let error =
        parse_launch_args(vec![OsString::from("sonicterm"), title_arg], CliDialect::Windows, None)
            .unwrap_err();
    assert_eq!(error.rule, UsageRule::NonUnicodeTitle);
}
