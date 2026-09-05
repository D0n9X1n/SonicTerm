use super::*;

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

#[test]
fn no_payload_and_unknown_args_are_ignored() {
    assert!(parse_cli_from(["sonicterm"]).unwrap().tearout.is_none());
    assert!(parse_cli_from(["sonicterm", "--unknown", "value"]).unwrap().tearout.is_none());
}

#[test]
fn missing_and_malformed_payloads_return_contextual_errors() {
    let err = parse_cli_from(["sonicterm", "--tear-out-payload"]).unwrap_err();
    assert!(err.to_string().contains("requires a JSON argument"));
    let err = parse_cli_from(["sonicterm", "--tear-out-payload", "not-json"]).unwrap_err();
    assert!(format!("{err:#}").contains("decode --tear-out-payload JSON"));
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
    let err = parse_cli_from(["sonicterm", "--open-script"]).unwrap_err();
    assert!(err.to_string().contains("requires a path argument"));

    let err = parse_cli_from(["sonicterm", "--open-script", "one.ps1", "--open-script", "two.ps1"])
        .unwrap_err();
    assert!(err.to_string().contains("may be provided only once"));

    let json = payload("one").to_json().unwrap();
    let err = parse_cli_from([
        std::ffi::OsString::from("sonicterm"),
        std::ffi::OsString::from("--open-script"),
        std::ffi::OsString::from("one.ps1"),
        std::ffi::OsString::from("--tear-out-payload"),
        std::ffi::OsString::from(json),
    ])
    .unwrap_err();
    assert!(err.to_string().contains("cannot be combined"));
}

#[test]
fn refresh_flag_is_parsed_without_changing_unknown_argument_tolerance() {
    let parsed =
        parse_cli_from(["sonicterm", "--unknown", "value", "--refresh-shell-associations"])
            .unwrap();
    assert!(parsed.refresh_shell_associations);
    assert!(parsed.open_script.is_none());
    assert!(parsed.tearout.is_none());
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
