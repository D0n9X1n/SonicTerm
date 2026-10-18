use super::*;
use sonicterm_app::launch_cli::UsageRule;

/// The usage rule of a refused command line.
fn usage_rule(result: Result<ParsedCli, CliError>) -> UsageRule {
    match result {
        Err(CliError::Usage(error)) => error.rule,
        other => panic!("expected a usage error, got {other:?}"),
    }
}

fn payload(title: &str) -> TabPayload {
    TabPayload {
        pty_pid: 42,
        tab_title: title.to_string(),
        scrollback_b64: TabPayload::encode_scrollback(b"hello"),
        cwd: "C:/work".to_string(),
        cmd: "pwsh.exe".to_string(),
        env: vec![("TERM".to_string(), "xterm-256color".to_string())],
    }
}

#[test]
fn valid_tearout_payload_parses_and_wrapper_matches() {
    let expected = payload("one");
    let json = expected.to_json().unwrap();
    let args = vec!["sonicterm", "--tear-out-payload", json.as_str()];
    assert_eq!(parse_cli_from(args.clone()).unwrap().tearout, Some(expected.clone()));
    assert_eq!(parse_tearout_payload_from(args).unwrap(), Some(expected));
}

/// Unknown options are usage errors (exit 2): no registered or spawned command line passes one.
#[test]
fn unknown_arguments_are_usage_errors() {
    assert!(parse_cli_from(["sonicterm"]).unwrap().tearout.is_none());
    assert_eq!(
        usage_rule(parse_cli_from(["sonicterm", "--unknown", "value"])),
        UsageRule::UnknownOption
    );
    assert_eq!(usage_rule(parse_cli_from(["sonicterm", "stray"])), UsageRule::Positional);
}

#[test]
fn missing_and_malformed_payloads_return_contextual_errors() {
    assert_eq!(
        usage_rule(parse_cli_from(["sonicterm", "--tear-out-payload"])),
        UsageRule::MissingValue
    );
    let err = parse_cli_from(["sonicterm", "--tear-out-payload", "not-json"]).unwrap_err();
    assert!(matches!(err, CliError::Payload(_)));
    assert!(err.to_string().contains("decode --tear-out-payload JSON"));
}

#[test]
fn repeated_payload_uses_the_last_value() {
    let first = payload("first");
    let last = payload("last");
    let first_json = first.to_json().unwrap();
    let last_json = last.to_json().unwrap();
    let args = vec![
        "sonicterm",
        "--tear-out-payload",
        first_json.as_str(),
        "--tear-out-payload",
        last_json.as_str(),
    ];
    assert_eq!(parse_cli_from(args).unwrap().tearout, Some(last));
}

#[test]
fn open_script_preserves_spaces_and_unicode_as_an_os_path() {
    let parsed = parse_cli_from(["sonicterm", "--open-script", "C:/work folder/脚本.ps1"]).unwrap();
    assert_eq!(parsed.open_script, Some(std::path::PathBuf::from("C:/work folder/脚本.ps1")));
}

#[test]
fn open_script_rejects_missing_repeated_and_tearout_conflicts() {
    assert_eq!(usage_rule(parse_cli_from(["sonicterm", "--open-script"])), UsageRule::MissingValue);
    assert_eq!(
        usage_rule(parse_cli_from([
            "sonicterm",
            "--open-script",
            "one.ps1",
            "--open-script",
            "two.ps1"
        ])),
        UsageRule::Repeated
    );

    let json = payload("one").to_json().unwrap();
    let err = parse_cli_from([
        std::ffi::OsString::from("sonicterm"),
        std::ffi::OsString::from("--open-script"),
        std::ffi::OsString::from("one.ps1"),
        std::ffi::OsString::from("--tear-out-payload"),
        std::ffi::OsString::from(json),
    ])
    .unwrap_err();
    assert!(matches!(err, CliError::Usage(error) if error.rule == UsageRule::Conflict));
}

/// The MSI's `--refresh-shell-associations` still parses alone, and refuses any companion.
#[test]
fn refresh_flag_parses_alone_and_refuses_companions() {
    let parsed = parse_cli_from(["sonicterm", "--refresh-shell-associations"]).unwrap();
    assert!(parsed.refresh_shell_associations);
    assert!(parsed.open_script.is_none());
    assert!(parsed.tearout.is_none());
    assert!(parsed.terminal.is_none());
    assert_eq!(
        usage_rule(parse_cli_from(["sonicterm", "--unknown", "--refresh-shell-associations"])),
        UsageRule::UnknownOption
    );
    assert_eq!(
        usage_rule(parse_cli_from(["sonicterm", "--refresh-shell-associations", "--hold"])),
        UsageRule::Conflict
    );
}

/// A command launch carries its program, arguments, title and directory unchanged.
#[test]
fn command_launch_parses_with_terminal_options() {
    let root = std::env::temp_dir();
    let parsed = parse_cli_from_with_cwd(
        ["sonicterm", "-T", "Build", "--hold", "-e", "cmd", "/c", "a b"],
        Some(&root),
    )
    .unwrap();
    let terminal = parsed.terminal.expect("terminal launch");
    assert_eq!(terminal.title.as_deref(), Some("Build"));
    assert!(terminal.hold);
    let command = terminal.command.expect("command");
    assert_eq!(command.program, "cmd");
    assert_eq!(command.args, ["/c", "a b"]);
}

/// `--help` is its own mode.
#[test]
fn help_is_parsed() {
    assert!(parse_cli_from(["sonicterm", "--help"]).unwrap().help);
}

#[test]
fn runtime_smoke_is_a_dedicated_startup_mode() {
    // Protect release automation from silently launching an interactive user session.
    let parsed = parse_cli_from(["sonicterm-windows", "--runtime-smoke"]).unwrap();
    assert!(parsed.runtime_smoke);
    for conflicting in [
        "--open-script",
        "--tear-out-payload",
        "--refresh-shell-associations",
        "--unknown",
        "--runtime-smoke",
    ] {
        let mut args = vec!["sonicterm-windows", "--runtime-smoke", conflicting];
        if conflicting == "--open-script" {
            args.push("script.ps1");
        } else if conflicting == "--tear-out-payload" {
            args.push("not-json");
        }
        assert!(parse_cli_from(args).is_err());
    }
}

#[cfg(unix)]
#[test]
fn open_script_preserves_non_utf8_os_strings() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let raw = b"/tmp/script-\xff.sh".to_vec();
    let parsed = parse_cli_from([
        std::ffi::OsString::from("sonicterm"),
        std::ffi::OsString::from("--open-script"),
        std::ffi::OsString::from_vec(raw.clone()),
    ])
    .unwrap();
    assert_eq!(parsed.open_script.unwrap().as_os_str().as_bytes(), raw);
}

/// A ProgID path with spaces, an accent and an unpaired surrogate stays exact through `--open-script`.
#[cfg(windows)]
#[test]
fn open_script_preserves_unpaired_surrogates() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let mut raw: Vec<u16> = r"C:\x y\é".encode_utf16().collect();
    raw.push(0xD800);
    raw.extend(".ps1".encode_utf16());
    let parsed = parse_cli_from([
        OsString::from("sonicterm"),
        OsString::from("--open-script"),
        OsString::from_wide(&raw),
    ])
    .unwrap();
    assert_eq!(parsed.open_script.unwrap().as_os_str().encode_wide().collect::<Vec<_>>(), raw);
}

/// Both refusal kinds display their reason; a payload error names the option it came from.
#[test]
fn cli_errors_display_their_reason() {
    let payload = CliError::Payload("bad json".to_string());
    assert_eq!(payload.to_string(), "decode --tear-out-payload JSON: bad json");
    let usage = parse_cli_from(["sonicterm", "--unknown"]).unwrap_err();
    assert!(matches!(usage, CliError::Usage(_)));
    assert!(!usage.to_string().is_empty());
}
