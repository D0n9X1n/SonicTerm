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
